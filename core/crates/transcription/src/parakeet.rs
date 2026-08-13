//! NVIDIA Parakeet (TDT) transcription.
//!
//! An alternative to Whisper. Parakeet is a FastConformer **transducer**, which differs from
//! Whisper in ways that matter here:
//!
//! - It is streaming-shaped. A transducer consumes encoder frames left to right and emits
//!   tokens as it goes, so there is no fixed decode window and no phrase cut in half by one.
//! - It does not hallucinate over silence the way an encoder-decoder does. Whisper is a
//!   language model conditioned on audio and will happily invent a fluent sentence for room
//!   tone; a transducer emits blank and moves on. The speech gate in [`crate::WhisperEngine`]
//!   exists for that failure and is not needed here.
//! - It is English-only, which is why it is an alternative rather than a replacement.
//!
//! # TDT, not plain RNNT
//!
//! Parakeet-TDT is a *Token-and-Duration* Transducer. The joiner emits two distributions: one
//! over the vocabulary and one over a small set of **durations**. After each step the decoder
//! skips forward by the predicted duration rather than always advancing one frame.
//!
//! That is the whole speed advantage, and it is also the easiest thing to get wrong. Decoding a
//! TDT model as if it were plain RNNT — always advancing one frame — produces text that is
//! recognisable but repeats tokens, because the frames the model expected to skip are decoded
//! again.
//!
//! # Three models, one engine
//!
//! - **encoder** — filterbank features to acoustic frames. 652 MB, and the only slow part.
//! - **decoder** — the prediction network, an LSTM over emitted tokens. Its state carries
//!   across the whole utterance.
//! - **joiner** — combines one encoder frame with the decoder state into token and duration
//!   logits.
//!
//! # Status: works, not yet trusted
//!
//! On the reference sample shipped with the model this transcribes exactly, with punctuation
//! and casing. On a synthetic text-to-speech sample it dropped a clause that
//! [`crate::WhisperEngine`] transcribed correctly from the same file — "Sam will own the index"
//! vanished while the rest of the sentence survived.
//!
//! One clause lost from a meeting is worse than a whole one, because nothing marks the gap.
//! Until that is understood this stays behind a feature flag and Whisper remains the default.
//! The most likely suspects, in order: the duration set may not be `[0,1,2,3,4]` — the joiner
//! emits 1030 logits over a 1025-token vocabulary, which is consistent with five durations but
//! does not prove their values — or the feature extraction may not match NeMo's preprocessor
//! closely enough on a signal that synthetic speech happens to stress.

use async_trait::async_trait;
#[cfg(feature = "parakeet")]
use notewise_audio_capture::AudioFormat;
use notewise_audio_capture::AudioFrame;

use crate::engine::TranscriptionEngine;
use crate::segment::Segment;
use crate::{Result, TranscriptionError};

/// Vocabulary and the blank symbol.
///
/// The blank is *not* a word — it is the transducer's "emit nothing and advance" symbol, and it
/// is always the last id. Treating it as a token puts `<blk>` in the transcript.
#[derive(Debug, Clone)]
pub struct Vocabulary {
    tokens: Vec<String>,
}

impl Vocabulary {
    /// Parse sherpa-onnx's `tokens.txt`: one `symbol id` pair per line.
    ///
    /// The symbol may itself contain spaces, so the id is split from the *end*. Splitting from
    /// the front silently truncates any token containing a space.
    pub fn parse(text: &str) -> Result<Self> {
        let mut tokens: Vec<(usize, String)> = Vec::new();

        for (line_number, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }

            let (symbol, id) = line.rsplit_once(char::is_whitespace).ok_or_else(|| {
                TranscriptionError::BadAudio(format!(
                    "tokens.txt line {}: expected 'symbol id'",
                    line_number + 1
                ))
            })?;

            let id: usize = id.trim().parse().map_err(|_| {
                TranscriptionError::BadAudio(format!(
                    "tokens.txt line {}: '{id}' is not an id",
                    line_number + 1
                ))
            })?;
            tokens.push((id, symbol.to_string()));
        }

        if tokens.is_empty() {
            return Err(TranscriptionError::BadAudio("tokens.txt is empty".into()));
        }

        tokens.sort_unstable_by_key(|(id, _)| *id);
        let highest = tokens.last().map(|(id, _)| *id).unwrap_or(0);

        // Indexed by id, so a gap would silently shift every later token.
        let mut table = vec![String::new(); highest + 1];
        for (id, symbol) in tokens {
            table[id] = symbol;
        }

        Ok(Self { tokens: table })
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// The blank id: always the last entry.
    pub fn blank(&self) -> usize {
        self.tokens.len().saturating_sub(1)
    }

    /// Join token ids into text.
    ///
    /// The vocabulary is SentencePiece, where `▁` marks a word start rather than a space. A
    /// naive join with spaces produces "▁the ▁quick"; a naive join without them glues words
    /// together.
    pub fn decode(&self, ids: &[usize]) -> String {
        let mut text = String::new();
        for id in ids {
            let Some(symbol) = self.tokens.get(*id) else {
                continue;
            };
            if let Some(rest) = symbol.strip_prefix('\u{2581}') {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(rest);
            } else {
                text.push_str(symbol);
            }
        }
        text.trim().to_string()
    }
}

/// Where the three model files and the vocabulary live.
#[derive(Debug, Clone)]
pub struct ParakeetPaths {
    pub encoder: std::path::PathBuf,
    pub decoder: std::path::PathBuf,
    pub joiner: std::path::PathBuf,
    pub tokens: std::path::PathBuf,
}

impl ParakeetPaths {
    /// The layout sherpa-onnx publishes: `encoder.onnx`, `decoder.onnx`, `joiner.onnx`,
    /// `tokens.txt` in one directory.
    pub fn in_directory(dir: impl AsRef<std::path::Path>) -> Self {
        let dir = dir.as_ref();
        Self {
            encoder: dir.join("encoder.onnx"),
            decoder: dir.join("decoder.onnx"),
            joiner: dir.join("joiner.onnx"),
            tokens: dir.join("tokens.txt"),
        }
    }

    /// Whether every file is present, so a caller can check before paying for a load.
    pub fn are_present(&self) -> bool {
        self.encoder.exists()
            && self.decoder.exists()
            && self.joiner.exists()
            && self.tokens.exists()
    }

    // Used by the feature-gated loader to name every absent file at once.
    #[cfg_attr(not(feature = "parakeet"), allow(dead_code))]
    fn missing(&self) -> Vec<&std::path::Path> {
        [
            self.encoder.as_path(),
            self.decoder.as_path(),
            self.joiner.as_path(),
            self.tokens.as_path(),
        ]
        .into_iter()
        .filter(|p| !p.exists())
        .collect()
    }
}

/// Feature extraction matching NeMo's preprocessor.
///
/// Deliberately spelled out rather than reusing the Kaldi defaults, because every one of these
/// differs from what the speaker-embedding models want, and a mismatch degrades output without
/// erroring.
pub fn nemo_features() -> notewise_audio_capture::FbankConfig {
    use notewise_audio_capture::{FbankConfig, Normalization, WindowType};

    FbankConfig {
        num_mel_bins: 128,
        window: WindowType::Hann,
        // NeMo's log_zero_guard_value.
        log_offset: 2f32.powi(-24),
        // NeMo normalises per feature: mean *and* standard deviation, per mel bin.
        normalization: Normalization::MeanAndStd,
        // NeMo works on -1.0..1.0 floats. The int16 scaling Kaldi models need would put every
        // feature about 20 too high here.
        input_scale: 1.0,
        low_freq_hz: 0.0,
        high_freq_hz: 8_000.0,
        ..FbankConfig::default()
    }
}

/// Parakeet.
pub struct ParakeetEngine {
    vocabulary: Vocabulary,
    #[allow(dead_code)] // Read only by the feature-gated inference path.
    extractor: notewise_audio_capture::FbankExtractor,
    /// Audio accumulated since the last decode.
    buffer: Vec<f32>,
    /// Milliseconds already decoded, for offsetting segment timings.
    offset_ms: i64,
    #[allow(dead_code)]
    window_samples: usize,

    #[cfg(feature = "parakeet")]
    models: std::sync::Mutex<Models>,
}

#[cfg(feature = "parakeet")]
struct Models {
    encoder: ort::session::Session,
    decoder: ort::session::Session,
    joiner: ort::session::Session,
}

impl std::fmt::Debug for ParakeetEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The sessions hold ~660 MB of weights and print nothing useful; audio must not reach
        // a log line.
        f.debug_struct("ParakeetEngine")
            .field("vocabulary", &self.vocabulary.len())
            .field("buffered_samples", &self.buffer.len())
            .field("offset_ms", &self.offset_ms)
            .finish()
    }
}

impl ParakeetEngine {
    /// How much audio to accumulate before decoding.
    ///
    /// Longer than Whisper's window because a transducer does not lose context at a boundary
    /// the way an encoder-decoder does — it emits as it goes — so the only cost of a longer
    /// window is latency to the first visible text.
    pub const WINDOW_SECONDS: usize = 15;

    /// Load the three models and the vocabulary.
    #[cfg(feature = "parakeet")]
    pub fn load(paths: &ParakeetPaths) -> Result<Self> {
        let missing = paths.missing();
        if !missing.is_empty() {
            return Err(TranscriptionError::BadAudio(format!(
                "missing Parakeet files: {}",
                missing
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }

        let vocabulary = Vocabulary::parse(&std::fs::read_to_string(&paths.tokens)?)?;

        let session = |path: &std::path::Path| -> Result<ort::session::Session> {
            ort::session::Session::builder()
                .map_err(|e| TranscriptionError::BadAudio(format!("session builder: {e}")))?
                .commit_from_file(path)
                .map_err(|e| {
                    TranscriptionError::BadAudio(format!("loading {}: {e}", path.display()))
                })
        };

        let window_samples =
            AudioFormat::transcription().sample_rate.hz() as usize * Self::WINDOW_SECONDS;

        Ok(Self {
            vocabulary,
            extractor: notewise_audio_capture::FbankExtractor::new(nemo_features()),
            buffer: Vec::with_capacity(window_samples),
            offset_ms: 0,
            window_samples,
            models: std::sync::Mutex::new(Models {
                encoder: session(&paths.encoder)?,
                decoder: session(&paths.decoder)?,
                joiner: session(&paths.joiner)?,
            }),
        })
    }

    #[cfg(not(feature = "parakeet"))]
    pub fn load(_paths: &ParakeetPaths) -> Result<Self> {
        Err(Self::unavailable())
    }

    #[cfg(not(feature = "parakeet"))]
    fn unavailable() -> TranscriptionError {
        TranscriptionError::EngineUnavailable {
            engine: "parakeet",
            reason: "built without the 'parakeet' feature",
        }
    }

    pub fn vocabulary(&self) -> &Vocabulary {
        &self.vocabulary
    }

    /// Decode whatever is buffered.
    #[cfg(feature = "parakeet")]
    fn decode(&mut self) -> Result<Vec<Segment>> {
        if self.buffer.is_empty() {
            return Ok(Vec::new());
        }

        let audio = std::mem::take(&mut self.buffer);
        let rate = AudioFormat::transcription().sample_rate.hz() as i64;
        let duration_ms = (audio.len() as i64 * 1000) / rate;

        let fbank = self.extractor.compute(&audio);
        if fbank.is_empty() {
            self.offset_ms += duration_ms;
            return Ok(Vec::new());
        }

        let start_ms = self.offset_ms;
        self.offset_ms += duration_ms;

        let text = {
            let mut models = self
                .models
                .lock()
                .map_err(|_| TranscriptionError::BadAudio("model session poisoned".into()))?;
            let encoder_out = run_encoder(&mut models.encoder, &fbank)?;
            decode_greedy(&mut models, &encoder_out, &self.vocabulary)?
        };

        if text.trim().is_empty() {
            return Ok(Vec::new());
        }

        Ok(vec![Segment::new(text.trim(), start_ms, self.offset_ms)])
    }
}

/// Run the encoder over one window's features.
#[cfg(feature = "parakeet")]
fn run_encoder(
    encoder: &mut ort::session::Session,
    fbank: &notewise_audio_capture::Fbank,
) -> Result<EncoderOutput> {
    use ort::value::Value;

    // NeMo's encoder takes [batch, mel_bins, frames] — features **transposed** relative to the
    // [batch, frames, bins] a speaker model wants. Feeding the wrong orientation is accepted
    // silently by ONNX whenever the two dimensions happen to be compatible and produces noise.
    let mut transposed = vec![0.0f32; fbank.frames * fbank.num_bins];
    for frame in 0..fbank.frames {
        for bin in 0..fbank.num_bins {
            transposed[bin * fbank.frames + frame] = fbank.data[frame * fbank.num_bins + bin];
        }
    }

    let features = Value::from_array(([1usize, fbank.num_bins, fbank.frames], transposed))
        .map_err(|e| TranscriptionError::BadAudio(format!("encoder input: {e}")))?;
    let lengths = Value::from_array(([1usize], vec![fbank.frames as i64]))
        .map_err(|e| TranscriptionError::BadAudio(format!("encoder lengths: {e}")))?;

    let input_names: Vec<String> = encoder
        .inputs()
        .iter()
        .map(|i| i.name().to_string())
        .collect();
    if input_names.len() < 2 {
        return Err(TranscriptionError::BadAudio(format!(
            "the encoder expects {} input(s); this decoder assumes features plus lengths",
            input_names.len()
        )));
    }

    let outputs = encoder
        .run(ort::inputs![
            input_names[0].as_str() => features,
            input_names[1].as_str() => lengths,
        ])
        .map_err(|e| TranscriptionError::BadAudio(format!("encoder inference: {e}")))?;

    let (_, encoded) = outputs
        .iter()
        .next()
        .ok_or_else(|| TranscriptionError::BadAudio("the encoder returned nothing".into()))?;
    let (shape, data) = encoded
        .try_extract_tensor::<f32>()
        .map_err(|e| TranscriptionError::BadAudio(format!("encoder output: {e}")))?;

    // [batch, features, frames] or [batch, frames, features]; NeMo emits the former.
    let dims: Vec<usize> = shape.iter().map(|d| *d as usize).collect();
    if dims.len() != 3 {
        return Err(TranscriptionError::BadAudio(format!(
            "expected a 3-D encoder output, got {dims:?}"
        )));
    }

    Ok(EncoderOutput {
        frames: dims[2],
        features: dims[1],
        data: data.to_vec(),
    })
}

/// Encoder output, kept as `[features][frames]` in the order NeMo emits.
#[cfg(feature = "parakeet")]
struct EncoderOutput {
    frames: usize,
    features: usize,
    data: Vec<f32>,
}

#[cfg(feature = "parakeet")]
impl EncoderOutput {
    /// One frame as a contiguous feature vector.
    fn frame(&self, index: usize) -> Vec<f32> {
        (0..self.features)
            .map(|f| self.data[f * self.frames + index])
            .collect()
    }
}

/// Greedy TDT decoding.
///
/// The loop is the part that differs from plain RNNT: the joiner returns vocabulary logits
/// *and* duration logits, and the frame pointer advances by the predicted duration rather than
/// by one. A duration of zero would spin forever on the same frame, so it is forced to at least
/// one after a blank.
#[cfg(feature = "parakeet")]
fn decode_greedy(
    models: &mut Models,
    encoder_out: &EncoderOutput,
    vocabulary: &Vocabulary,
) -> Result<String> {
    let blank = vocabulary.blank();
    let vocab_size = vocabulary.len();

    let mut emitted: Vec<usize> = Vec::new();
    // A transducer starts from the blank token and a zero state.
    let initial = DecoderState::initial(&models.decoder);
    let mut decoder_state = decoder_step(&mut models.decoder, blank, &initial)?;

    let mut t = 0usize;
    // A transducer can emit several tokens on one frame. Bounded so a degenerate model cannot
    // loop forever on a single frame of audio.
    let mut symbols_on_this_frame = 0usize;
    const MAX_SYMBOLS_PER_FRAME: usize = 10;
    // Bounds total work: at most a few tokens per frame across the whole window.
    let budget = encoder_out.frames * MAX_SYMBOLS_PER_FRAME + 64;
    let mut steps = 0usize;

    while t < encoder_out.frames && steps < budget {
        steps += 1;

        let logits = joiner_step(
            &mut models.joiner,
            &encoder_out.frame(t),
            &decoder_state.output,
        )?;

        // The joiner's output is [vocabulary | durations]; anything past the vocabulary is the
        // duration distribution.
        let (token_logits, duration_logits) = logits.split_at(vocab_size.min(logits.len()));

        let token = argmax(token_logits);
        let duration = if duration_logits.is_empty() {
            1
        } else {
            argmax(duration_logits)
        };

        if token == blank {
            // Blank: advance and reset the per-frame counter. A zero duration here would never
            // move the pointer.
            t += duration.max(1);
            symbols_on_this_frame = 0;
            continue;
        }

        emitted.push(token);
        decoder_state = decoder_step(&mut models.decoder, token, &decoder_state)?;
        symbols_on_this_frame += 1;

        // A non-blank with a positive duration also advances; with duration zero the model is
        // asking to emit again on the same frame, which is allowed but bounded.
        if duration > 0 {
            t += duration;
            symbols_on_this_frame = 0;
        } else if symbols_on_this_frame >= MAX_SYMBOLS_PER_FRAME {
            t += 1;
            symbols_on_this_frame = 0;
        }
    }

    Ok(vocabulary.decode(&emitted))
}

/// The decoder's LSTM state, carried across the whole utterance.
///
/// Two tensors of `[2, batch, 640]` — the LSTM's hidden and cell state. The graph names them
/// `states.1` and `onnx::Slice_3` on the way in and `states` and `162` on the way out; those
/// are export artefacts, not meaningful names, so they are bound by position.
#[cfg(feature = "parakeet")]
struct DecoderState {
    /// `[1, 640, 1]` flattened: the prediction network output for the last token.
    output: Vec<f32>,
    hidden: Vec<f32>,
    cell: Vec<f32>,
    state_shape: Vec<usize>,
}

#[cfg(feature = "parakeet")]
impl DecoderState {
    /// The zero state a transducer starts from.
    fn initial(decoder: &ort::session::Session) -> Self {
        // Read from the graph rather than hard-coded: the LSTM width differs between Parakeet
        // sizes, and a wrong constant here is a shape error at the first token.
        let shape: Vec<usize> = decoder
            .inputs()
            .get(2)
            .and_then(|input| input.dtype().tensor_shape())
            .map(|s| {
                s.iter()
                    .map(|d| if *d > 0 { *d as usize } else { 1 })
                    .collect()
            })
            .unwrap_or_else(|| vec![2, 1, 640]);

        let count: usize = shape.iter().product();
        Self {
            output: Vec::new(),
            hidden: vec![0.0; count],
            cell: vec![0.0; count],
            state_shape: shape,
        }
    }
}

/// Advance the prediction network by one token.
#[cfg(feature = "parakeet")]
fn decoder_step(
    decoder: &mut ort::session::Session,
    token: usize,
    state: &DecoderState,
) -> Result<DecoderState> {
    use ort::value::Value;

    let names: Vec<String> = decoder
        .inputs()
        .iter()
        .map(|i| i.name().to_string())
        .collect();
    if names.len() < 4 {
        return Err(TranscriptionError::BadAudio(format!(
            "the decoder declares {} inputs; this loop needs targets, target_length and two \
             state tensors",
            names.len()
        )));
    }

    // targets [1, 1] and target_length [1] are Int32, not Int64 — the encoder's `length` is
    // Int64 and the decoder's is not, which is the kind of asymmetry that only shows up as a
    // runtime type error.
    let targets = Value::from_array(([1usize, 1usize], vec![token as i32]))
        .map_err(|e| TranscriptionError::BadAudio(format!("decoder targets: {e}")))?;
    let target_length = Value::from_array(([1usize], vec![1i32]))
        .map_err(|e| TranscriptionError::BadAudio(format!("decoder target_length: {e}")))?;
    let hidden = Value::from_array((state.state_shape.clone(), state.hidden.clone()))
        .map_err(|e| TranscriptionError::BadAudio(format!("decoder hidden state: {e}")))?;
    let cell = Value::from_array((state.state_shape.clone(), state.cell.clone()))
        .map_err(|e| TranscriptionError::BadAudio(format!("decoder cell state: {e}")))?;

    let outputs = decoder
        .run(ort::inputs![
            names[0].as_str() => targets,
            names[1].as_str() => target_length,
            names[2].as_str() => hidden,
            names[3].as_str() => cell,
        ])
        .map_err(|e| TranscriptionError::BadAudio(format!("decoder inference: {e}")))?;

    let extract = |index: usize, what: &str| -> Result<Vec<f32>> {
        let (_, value) = outputs.iter().nth(index).ok_or_else(|| {
            TranscriptionError::BadAudio(format!("the decoder returned no {what}"))
        })?;
        let (_, data) = value
            .try_extract_tensor::<f32>()
            .map_err(|e| TranscriptionError::BadAudio(format!("decoder {what}: {e}")))?;
        Ok(data.to_vec())
    };

    // Outputs are (prediction, prednet_lengths, new hidden, new cell). Index 1 is Int32 and is
    // not read, which is why the state indices are 2 and 3 rather than 1 and 2.
    Ok(DecoderState {
        output: extract(0, "prediction")?,
        hidden: extract(2, "hidden state")?,
        cell: extract(3, "cell state")?,
        state_shape: state.state_shape.clone(),
    })
}

/// One joiner step: one encoder frame plus the decoder output to logits.
#[cfg(feature = "parakeet")]
fn joiner_step(
    joiner: &mut ort::session::Session,
    encoder_frame: &[f32],
    decoder_output: &[f32],
) -> Result<Vec<f32>> {
    use ort::value::Value;

    let names: Vec<String> = joiner
        .inputs()
        .iter()
        .map(|i| i.name().to_string())
        .collect();

    // Both inputs are 3-D — [batch, features, time] with a time extent of one. A 2-D tensor is
    // rejected outright, which is the good case; the bad case is a rank that happens to
    // broadcast and produces logits for the wrong thing.
    let encoder = Value::from_array((
        [1usize, encoder_frame.len(), 1usize],
        encoder_frame.to_vec(),
    ))
    .map_err(|e| TranscriptionError::BadAudio(format!("joiner encoder input: {e}")))?;
    let decoder = Value::from_array((
        [1usize, decoder_output.len(), 1usize],
        decoder_output.to_vec(),
    ))
    .map_err(|e| TranscriptionError::BadAudio(format!("joiner decoder input: {e}")))?;

    let outputs = joiner
        .run(ort::inputs![
            names[0].as_str() => encoder,
            names[1].as_str() => decoder,
        ])
        .map_err(|e| TranscriptionError::BadAudio(format!("joiner inference: {e}")))?;

    let (_, joined) = outputs
        .iter()
        .next()
        .ok_or_else(|| TranscriptionError::BadAudio("the joiner returned nothing".into()))?;
    let (_, logits) = joined
        .try_extract_tensor::<f32>()
        .map_err(|e| TranscriptionError::BadAudio(format!("joiner output: {e}")))?;

    Ok(logits.to_vec())
}

/// Index of the largest value. Ties go to the first, which keeps decoding deterministic.
#[cfg(feature = "parakeet")]
fn argmax(values: &[f32]) -> usize {
    values
        .iter()
        .enumerate()
        .fold((0usize, f32::NEG_INFINITY), |(best_i, best), (i, v)| {
            if *v > best {
                (i, *v)
            } else {
                (best_i, best)
            }
        })
        .0
}

#[async_trait]
impl TranscriptionEngine for ParakeetEngine {
    fn name(&self) -> &str {
        "parakeet-tdt"
    }

    #[cfg(feature = "parakeet")]
    async fn feed(&mut self, frame: &AudioFrame) -> Result<Vec<Segment>> {
        if frame.format != self.required_format() {
            return Err(TranscriptionError::BadAudio(format!(
                "expected {}, got {}",
                self.required_format(),
                frame.format
            )));
        }

        self.buffer.extend_from_slice(&frame.samples);
        if self.buffer.len() >= self.window_samples {
            return self.decode();
        }
        Ok(Vec::new())
    }

    #[cfg(not(feature = "parakeet"))]
    async fn feed(&mut self, _frame: &AudioFrame) -> Result<Vec<Segment>> {
        Err(Self::unavailable())
    }

    #[cfg(feature = "parakeet")]
    async fn finish(&mut self) -> Result<Vec<Segment>> {
        self.decode()
    }

    #[cfg(not(feature = "parakeet"))]
    async fn finish(&mut self) -> Result<Vec<Segment>> {
        Err(Self::unavailable())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocabulary() -> Vocabulary {
        Vocabulary::parse("<unk> 0\n\u{2581}the 1\n\u{2581}quick 2\nly 3\n<blk> 4\n")
            .expect("vocabulary")
    }

    // ------------------------------------------------------------------ vocabulary

    #[test]
    fn tokens_are_indexed_by_id_not_by_line_order() {
        // Deliberately out of order: a parser that trusts line order would map these wrongly.
        let vocabulary = Vocabulary::parse("b 2\na 1\n<blk> 3\nx 0\n").expect("parse");
        assert_eq!(vocabulary.decode(&[0]), "x");
        assert_eq!(vocabulary.decode(&[1]), "a");
        assert_eq!(vocabulary.decode(&[2]), "b");
    }

    #[test]
    fn the_blank_is_the_last_id() {
        assert_eq!(vocabulary().blank(), 4);
    }

    /// `▁` marks a word start, not a space. Joining naively either leaves the marker in the
    /// transcript or glues every word together.
    #[test]
    fn word_start_markers_become_spaces() {
        assert_eq!(vocabulary().decode(&[1, 2, 3]), "the quickly");
    }

    #[test]
    fn a_leading_word_marker_does_not_produce_a_leading_space() {
        assert_eq!(vocabulary().decode(&[1]), "the");
    }

    #[test]
    fn continuation_pieces_attach_without_a_space() {
        assert_eq!(vocabulary().decode(&[2, 3]), "quick".to_owned() + "ly");
    }

    /// A symbol may contain a space, so the id has to be split from the end of the line.
    #[test]
    fn symbols_containing_spaces_survive_parsing() {
        let vocabulary = Vocabulary::parse("a b 0\n<blk> 1\n").expect("parse");
        assert_eq!(vocabulary.decode(&[0]), "a b");
    }

    #[test]
    fn an_empty_vocabulary_is_rejected() {
        assert!(Vocabulary::parse("").is_err());
        assert!(Vocabulary::parse("   \n\n").is_err());
    }

    #[test]
    fn a_malformed_line_is_rejected_with_its_number() {
        let error = Vocabulary::parse("ok 0\nbroken\n").expect_err("should fail");
        assert!(error.to_string().contains("line 2"), "{error}");
    }

    #[test]
    fn a_non_numeric_id_is_rejected() {
        assert!(Vocabulary::parse("a zero\n").is_err());
    }

    #[test]
    fn unknown_ids_are_skipped_rather_than_panicking() {
        // A model emitting an out-of-range id is a bug, but it must not take the app down
        // mid-meeting.
        assert_eq!(vocabulary().decode(&[1, 999, 2]), "the quick");
    }

    // ------------------------------------------------------------------ paths

    #[test]
    fn the_sherpa_layout_is_resolved() {
        let paths = ParakeetPaths::in_directory("/models/parakeet");
        assert!(paths.encoder.ends_with("encoder.onnx"));
        assert!(paths.decoder.ends_with("decoder.onnx"));
        assert!(paths.joiner.ends_with("joiner.onnx"));
        assert!(paths.tokens.ends_with("tokens.txt"));
        assert!(!paths.are_present());
    }

    #[test]
    fn loading_names_every_missing_file() {
        let paths = ParakeetPaths::in_directory("/definitely/not/here");
        let error = ParakeetEngine::load(&paths).expect_err("should fail");
        let message = error.to_string();
        // Either the unavailable message, or a list of what is missing.
        assert!(
            message.contains("parakeet") || message.contains("encoder.onnx"),
            "{message}"
        );
    }

    // ------------------------------------------------------------------ features

    /// Parakeet's preprocessing differs from the speaker models' on every axis that matters.
    /// Sharing one config would silently degrade one of them.
    #[test]
    fn nemo_features_differ_from_the_kaldi_defaults() {
        use notewise_audio_capture::{FbankConfig, Normalization, WindowType};

        let nemo = nemo_features();
        let kaldi = FbankConfig::default();

        assert_eq!(nemo.num_mel_bins, 128, "NeMo uses 128 mel bins");
        assert_eq!(nemo.window, WindowType::Hann, "not Povey");
        assert_eq!(nemo.normalization, Normalization::MeanAndStd);
        assert_eq!(nemo.input_scale, 1.0, "NeMo works on -1..1 floats");
        assert!(
            nemo.log_offset > 0.0,
            "NeMo offsets the log rather than flooring it"
        );

        assert_ne!(nemo.window, kaldi.window);
        assert_ne!(nemo.input_scale, kaldi.input_scale);
        assert_ne!(nemo.num_mel_bins, kaldi.num_mel_bins);
    }

    #[cfg(feature = "parakeet")]
    #[test]
    fn argmax_returns_the_first_of_equal_maxima() {
        assert_eq!(argmax(&[0.1, 0.9, 0.3]), 1);
        assert_eq!(argmax(&[0.5, 0.5]), 0, "ties must be deterministic");
        assert_eq!(argmax(&[]), 0);
        assert_eq!(argmax(&[-3.0, -1.0, -2.0]), 1, "all-negative logits");
    }

    // ------------------------------------------------------------------ real inference

    /// Print each model's declared inputs and outputs.
    ///
    /// The three graphs' signatures are the thing this engine is written against, and guessing
    /// them is how a decoder ends up silently feeding a transposed tensor. Run this first when
    /// pointing the engine at a new model.
    ///
    /// `NOTEWISE_PARAKEET_DIR=... cargo test -p notewise-transcription \
    ///   --features parakeet-download -- --ignored --nocapture`
    #[cfg(feature = "parakeet")]
    #[test]
    #[ignore = "requires downloaded Parakeet models"]
    fn print_the_model_signatures() {
        let dir = std::env::var("NOTEWISE_PARAKEET_DIR").expect("NOTEWISE_PARAKEET_DIR");
        let paths = ParakeetPaths::in_directory(&dir);

        for (label, path) in [
            ("encoder", &paths.encoder),
            ("decoder", &paths.decoder),
            ("joiner", &paths.joiner),
        ] {
            let session = ort::session::Session::builder()
                .expect("builder")
                .commit_from_file(path)
                .expect("load");

            println!("\n=== {label} ===");
            for input in session.inputs() {
                println!("  in  {:<20} {:?}", input.name(), input.dtype());
            }
            for output in session.outputs() {
                println!("  out {:<20} {:?}", output.name(), output.dtype());
            }
        }
    }

    /// Real transcription of the sample sherpa-onnx ships with the model.
    ///
    /// `NOTEWISE_PARAKEET_DIR=... NOTEWISE_PARAKEET_WAV=... cargo test \
    ///   -p notewise-transcription --features parakeet-download -- --ignored --nocapture`
    #[cfg(feature = "parakeet")]
    #[tokio::test]
    #[ignore = "requires downloaded Parakeet models and a speech sample"]
    async fn transcribes_real_speech() {
        let dir = std::env::var("NOTEWISE_PARAKEET_DIR").expect("NOTEWISE_PARAKEET_DIR");
        let wav = std::env::var("NOTEWISE_PARAKEET_WAV").expect("NOTEWISE_PARAKEET_WAV");

        let mut engine = ParakeetEngine::load(&ParakeetPaths::in_directory(&dir)).expect("load");
        println!("{engine:?}");
        println!(
            "vocabulary: {} tokens, blank {}",
            engine.vocabulary().len(),
            engine.vocabulary().blank()
        );

        let mut source = notewise_audio_capture::FileSource::open_wav(&wav).expect("wav");
        let transcript = engine
            .transcribe_all(&mut source)
            .await
            .expect("transcribe");

        println!("\n--- transcript ---\n{}\n", transcript.to_text());

        let text = transcript.to_text();
        assert!(!text.trim().is_empty(), "produced no text at all");
        assert!(
            !text.contains("<blk>") && !text.contains('\u{2581}'),
            "raw vocabulary symbols leaked into the transcript: {text}"
        );

        // A transducer decoded as plain RNNT produces recognisable text with tokens repeated.
        // Crude but effective: no word should appear many times in a row.
        let words: Vec<&str> = text.split_whitespace().collect();
        let mut longest_run = 1usize;
        let mut run = 1usize;
        for pair in words.windows(2) {
            run = if pair[0] == pair[1] { run + 1 } else { 1 };
            longest_run = longest_run.max(run);
        }
        assert!(
            longest_run < 4,
            "a word repeated {longest_run} times in a row, which is what decoding a TDT model \
             as plain RNNT looks like: {text}"
        );
    }
}
