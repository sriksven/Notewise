# Scripts

| Script | Purpose |
|---|---|
| `download-models.sh` | Prefetch a Whisper model into the local model store |

`download-models.sh` duplicates what the CLI does on demand. It exists for installers and CI,
where a 148 MB download at first use is a bad first impression.
