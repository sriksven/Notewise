import { useEffect, useState } from "react";
import { AlertTriangle, CalendarClock, Check, ExternalLink, Loader2, RefreshCw } from "lucide-react";

import { api, ApiError, type AvailableConnector } from "../lib/api";

interface Props {
  connectors: AvailableConnector[];
  onChanged: () => void | Promise<void>;
}

/**
 * Connecting a calendar, and the mailbox that comes with it.
 *
 * # Why the two vendors look nothing alike
 *
 * Microsoft is a sign-in. Google is five steps of pasting, and that asymmetry is not an oversight —
 * it is the price of not paying Google. Every Gmail write scope is *restricted*, which means OAuth
 * verification plus an annually-billed third-party security assessment before a single line of
 * calendar code ships value. The way around it is a script the user deploys into their own account,
 * which runs as them and needs no review from anybody.
 *
 * So the setup says that, rather than presenting two buttons that behave differently for reasons the
 * user is left to guess at.
 *
 * # Mail is opt-in, separately
 *
 * A user who wants their calendar read does not necessarily want Notewise able to write drafts into
 * their mailbox. Those are one authorization at the vendor and two decisions here.
 */
export function CalendarSetup({ connectors, onChanged }: Props) {
  const google = connectors.find((c) => c.id === "google");
  const microsoft = connectors.find((c) => c.id === "microsoft");

  const [error, setError] = useState<string | null>(null);
  const [syncing, setSyncing] = useState(false);
  const [synced, setSynced] = useState<string | null>(null);

  // Anything to show at all?
  if (!google && !microsoft) return null;

  const sync = async () => {
    setSyncing(true);
    setError(null);
    try {
      const report = await api.syncConnectors();
      setSynced(
        report.failures.length > 0
          ? report.failures.join("; ")
          : `${report.upserted} event${report.upserted === 1 ? "" : "s"} up to date.`,
      );
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not sync.");
    } finally {
      setSyncing(false);
      await onChanged();
    }
  };

  const anyConnected = Boolean(google?.connected || microsoft?.connected);

  return (
    <section className="mt-7">
      <h2 className="mb-1 flex items-center gap-1.5 text-[12.5px] font-semibold text-ink">
        <CalendarClock size={13} className="text-ink-faint" aria-hidden />
        Calendar and mail
      </h2>
      <p className="mb-3 text-[12px] leading-relaxed text-ink-muted">
        Connecting a calendar gives meetings their real titles, links a recording to the event it
        belongs to, and turns attendees into names your transcripts can use. Notewise never writes to
        your calendar, and never sends mail — a follow-up becomes a draft in your own mailbox that
        you send yourself.
      </p>

      {error && (
        <div
          role="alert"
          className="mb-3 rounded-lg border border-warn-line bg-warn px-3 py-2 text-[12.5px] text-warn-text"
        >
          {error}
        </div>
      )}

      <div className="space-y-3">
        {microsoft && <MicrosoftCard connector={microsoft} onChanged={onChanged} />}
        {google && <GoogleCard connector={google} onChanged={onChanged} />}
      </div>

      {anyConnected && (
        <div className="mt-2.5 flex items-center gap-2">
          <button
            type="button"
            onClick={() => void sync()}
            disabled={syncing}
            className="flex items-center gap-1.5 rounded-full border border-hairline px-2.5 py-1
                       text-[11.5px] text-ink-muted transition hover:bg-overlay hover:text-ink
                       disabled:opacity-50"
          >
            {syncing ? (
              <Loader2 size={11} className="animate-spin" aria-hidden />
            ) : (
              <RefreshCw size={11} aria-hidden />
            )}
            Sync now
          </button>
          <p className="min-w-0 flex-1 truncate text-[11.5px] text-ink-faint">
            {synced ?? "Nothing pulls on a schedule yet, so events arrive when you ask."}
          </p>
        </div>
      )}
    </section>
  );
}

/** Sign in, and pick whether Notewise may draft mail. */
function MicrosoftCard({
  connector,
  onChanged,
}: {
  connector: AvailableConnector;
  onChanged: () => void | Promise<void>;
}) {
  const [clientId, setClientId] = useState("");
  const [mail, setMail] = useState(false);
  const [status, setStatus] = useState<"idle" | "pending" | "connected" | "failed">("idle");
  const [error, setError] = useState<string | null>(null);

  // Only while a sign-in is in flight: the answer arrives on a loopback redirect the engine catches,
  // so this is the one place polling is the right shape.
  useEffect(() => {
    if (status !== "pending") return;

    const timer = window.setInterval(() => {
      void api
        .microsoftSignInStatus()
        .then(async (found) => {
          if (found.state === "pending") return;
          setStatus(found.state);
          setError(found.error);
          if (found.state === "connected") await onChanged();
        })
        .catch(() => {});
    }, 1500);

    return () => window.clearInterval(timer);
  }, [status, onChanged]);

  const signIn = async () => {
    setError(null);
    try {
      const started = await api.startMicrosoftSignIn({
        client_id: clientId.trim() || undefined,
        scopes: mail ? ["calendar", "mail"] : ["calendar"],
      });
      setStatus("pending");
      // The engine is already listening on the redirect, so opening this cannot race the bind.
      window.open(started.authorize_url, "_blank", "noopener");
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not start signing in.");
    }
  };

  return (
    <div className="card p-3">
      <p className="flex items-center gap-1.5 text-[12.5px] text-ink">
        Microsoft 365
        {connector.connected && (
          <span className="flex items-center gap-0.5 rounded-full bg-ok px-1.5 py-0.5 text-[10.5px] text-ok-text">
            <Check size={9} strokeWidth={3} aria-hidden />
            connected
          </span>
        )}
      </p>
      <p className="mt-0.5 text-[11.5px] leading-relaxed text-ink-faint">
        One sign-in. Notewise asks for read access to your calendar, and to write drafts only if you
        tick the box — never to send.
      </p>

      {!connector.connected && (
        <div className="mt-2.5 space-y-2">
          <label className="block">
            <span className="mb-1 block text-[11.5px] text-ink-muted">
              Client id of an app registration in your tenant
            </span>
            <input
              value={clientId}
              onChange={(event) => setClientId(event.target.value)}
              placeholder="00000000-0000-0000-0000-000000000000"
              spellCheck={false}
              className="w-full rounded border border-hairline bg-transparent px-2 py-1 font-mono
                         text-[12px] text-ink placeholder:text-ink-faint"
            />
            <span className="mt-1 block text-[11px] leading-relaxed text-ink-faint">
              Notewise has no Microsoft app registration of its own yet, so this needs one of yours:
              Azure Portal → App registrations → New, with a <em>public client</em> redirect of{" "}
              <code>http://localhost</code>. No secret — there is nothing to leak in a desktop app.
            </span>
          </label>

          <label className="flex cursor-pointer items-start gap-2">
            <input
              type="checkbox"
              checked={mail}
              onChange={(event) => setMail(event.target.checked)}
              className="mt-0.5 h-3.5 w-3.5 accent-[var(--accent)]"
            />
            <span className="text-[11.5px] leading-relaxed text-ink-muted">
              Also let Notewise put follow-up drafts in my mailbox. Drafts only — the send endpoint
              is not in the code.
            </span>
          </label>

          <button
            type="button"
            onClick={() => void signIn()}
            disabled={status === "pending"}
            className="flex items-center gap-1.5 rounded-full bg-accent px-2.5 py-1 text-[11.5px]
                       text-accent-on transition hover:opacity-90 disabled:opacity-50"
          >
            {status === "pending" ? (
              <Loader2 size={11} className="animate-spin" aria-hidden />
            ) : (
              <ExternalLink size={11} aria-hidden />
            )}
            {status === "pending" ? "Waiting for you to sign in" : "Sign in with Microsoft"}
          </button>

          {status === "pending" && (
            <p className="text-[11.5px] leading-relaxed text-ink-faint">
              A page opened in your browser. Finish there and this will notice — you can close the
              tab afterwards.
            </p>
          )}

          {(error || status === "failed") && (
            <p className="flex items-start gap-1.5 text-[11.5px] leading-relaxed text-warn-text">
              <AlertTriangle size={12} className="mt-0.5 shrink-0" aria-hidden />
              {error ?? "That sign-in did not complete."}
            </p>
          )}
        </div>
      )}
    </div>
  );
}

/** Paste a deployment URL and the key you chose. */
function GoogleCard({
  connector,
  onChanged,
}: {
  connector: AvailableConnector;
  onChanged: () => void | Promise<void>;
}) {
  const [url, setUrl] = useState("");
  const [key, setKey] = useState("");
  const [mail, setMail] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showHow, setShowHow] = useState(false);

  const connect = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.connectConnector("google", url.trim(), {
        key: key.trim(),
        scopes: mail ? ["calendar", "mail"] : ["calendar"],
      });
      setUrl("");
      setKey("");
      await onChanged();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not connect that.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="card p-3">
      <p className="flex items-center gap-1.5 text-[12.5px] text-ink">
        Google
        {connector.connected && (
          <span className="flex items-center gap-0.5 rounded-full bg-ok px-1.5 py-0.5 text-[10.5px] text-ok-text">
            <Check size={9} strokeWidth={3} aria-hidden />
            connected
          </span>
        )}
      </p>
      <p className="mt-0.5 text-[11.5px] leading-relaxed text-ink-faint">
        A one-time setup, and it is longer than Microsoft&rsquo;s on purpose. Google classes every
        Gmail write as a restricted scope, which needs a paid annual security assessment before an
        app can ask for it. Instead you deploy a small open script into your own account: it runs as
        you, it needs nobody&rsquo;s review, and you can read every line of it first.
      </p>

      {!connector.connected && (
        <div className="mt-2.5 space-y-2">
          <button
            type="button"
            onClick={() => setShowHow((current) => !current)}
            className="text-[11.5px] text-accent underline-offset-2 hover:underline"
          >
            {showHow ? "Hide the five steps" : "Show me the five steps"}
          </button>

          {showHow && (
            <ol className="ml-4 list-decimal space-y-1 text-[11.5px] leading-relaxed text-ink-muted">
              <li>
                Open <code>script.google.com</code> and make a new project.
              </li>
              <li>
                Paste in the contents of <code>scripts/gapps/Code.gs</code> from the Notewise
                repository.
              </li>
              <li>
                Change <code>SHARED_KEY</code> at the top to a long random string, and keep it.
              </li>
              <li>
                Deploy → New deployment → Web app, executing as <em>you</em>, with access set to{" "}
                <em>Only myself</em>. Approve the permissions it asks for.
              </li>
              <li>Copy the deployment URL, and paste both below.</li>
            </ol>
          )}

          <input
            value={url}
            onChange={(event) => setUrl(event.target.value)}
            placeholder="https://script.google.com/macros/s/…/exec"
            spellCheck={false}
            className="w-full rounded border border-hairline bg-transparent px-2 py-1 font-mono
                       text-[12px] text-ink placeholder:text-ink-faint"
          />
          <input
            value={key}
            onChange={(event) => setKey(event.target.value)}
            type="password"
            placeholder="The SHARED_KEY you chose"
            spellCheck={false}
            className="w-full rounded border border-hairline bg-transparent px-2 py-1 font-mono
                       text-[12px] text-ink placeholder:text-ink-faint"
          />
          <p className="text-[11px] text-ink-faint">
            The key goes to your system keychain, never to the workspace database.
          </p>

          <label className="flex cursor-pointer items-start gap-2">
            <input
              type="checkbox"
              checked={mail}
              onChange={(event) => setMail(event.target.checked)}
              className="mt-0.5 h-3.5 w-3.5 accent-[var(--accent)]"
            />
            <span className="text-[11.5px] leading-relaxed text-ink-muted">
              Also let Notewise put follow-up drafts in Gmail. The script we ship has no send
              action at all, so this cannot send even if something asked it to.
            </span>
          </label>

          <button
            type="button"
            onClick={() => void connect()}
            disabled={busy || !url.trim() || !key.trim()}
            className="flex items-center gap-1.5 rounded-full bg-accent px-2.5 py-1 text-[11.5px]
                       text-accent-on transition hover:opacity-90 disabled:opacity-50"
          >
            {busy && <Loader2 size={11} className="animate-spin" aria-hidden />}
            Connect
          </button>

          {error && (
            <p className="flex items-start gap-1.5 text-[11.5px] leading-relaxed text-warn-text">
              <AlertTriangle size={12} className="mt-0.5 shrink-0" aria-hidden />
              {error}
            </p>
          )}
        </div>
      )}
    </div>
  );
}
