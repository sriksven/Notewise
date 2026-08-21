import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, Check, Copy, HardDrive, Loader2, Plug, Globe } from "lucide-react";

import {
  api,
  ApiError,
  type AvailableConnector,
  type FailedDelivery,
} from "../lib/api";
import { VaultDivergences } from "./VaultDivergences";

/**
 * Where meetings can be sent.
 *
 * This is the app's answer to a marketplace, and the difference is worth stating: there is no
 * catalogue served from anywhere and nothing to install. What is listed is exactly what this
 * binary contains, because a connector the build has no code for is one that would appear to
 * connect and then silently deliver nothing.
 *
 * Two of them, and the page says which one leaves the machine. That is the fact a user of a
 * local-first tool is entitled to see before they turn something on, not after.
 */
export function ConnectorsView() {
  const [connectors, setConnectors] = useState<AvailableConnector[]>([]);
  const [failures, setFailures] = useState<FailedDelivery[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const [available, failed] = await Promise.all([
        api.availableConnectors(),
        api.connectorFailures().catch(() => []),
      ]);
      setConnectors(available);
      setFailures(failed);
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not read the connector list.");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex items-center gap-3 border-b border-hairline px-8 py-3">
        <Plug size={16} className="shrink-0 text-ink-faint" aria-hidden />
        <h1 className="text-[14px] font-semibold text-ink">Connectors</h1>
        <span className="flex-1 text-[12px] text-ink-faint">
          {loading ? "Loading…" : `${connectors.filter((c) => c.connected).length} connected`}
        </span>
      </header>

      {error && (
        <p role="alert" className="border-b border-warn-line bg-warn px-8 py-2 text-[12.5px] text-warn-text">
          {error}
        </p>
      )}

      <div className="min-h-0 flex-1 overflow-y-auto px-8 py-6">
        <div className="mx-auto max-w-2xl">
          <p className="mb-5 text-[12.5px] leading-relaxed text-ink-muted">
            Everything below ships inside this build. There is nothing to install and no
            catalogue to browse — a connector this binary has no code for would appear to
            connect and then deliver nothing.
          </p>

          {loading ? (
            <p className="flex items-center gap-2 py-8 text-[12.5px] text-ink-faint">
              <Loader2 size={14} className="animate-spin" aria-hidden />
              Loading
            </p>
          ) : (
            <div className="space-y-3">
              {connectors.map((connector) => (
                <ConnectorCard key={connector.id} connector={connector} onChanged={load} />
              ))}
            </div>
          )}

          {/* Above failed deliveries, because a divergence is not a failure — it is the vault
              keeping its promise, and it needs an answer rather than a retry. */}
          <div className="mt-7">
            <VaultDivergences />
          </div>

          {failures.length > 0 && (
            <section className="mt-7">
              <h2 className="mb-2 flex items-center gap-1.5 text-[12.5px] font-semibold text-ink">
                <AlertTriangle size={13} className="text-warn-text" aria-hidden />
                Failed deliveries
              </h2>
              {/* Surfaced rather than buried in a log. A queue whose failures are invisible is
                  worse than no queue: the user believes their notes are in their vault. */}
              <ul className="card divide-y divide-hairline overflow-hidden">
                {failures.map((failure) => (
                  <li key={failure.id} className="px-4 py-2.5">
                    <p className="text-[12.5px] text-ink">
                      {failure.connector_id} · {failure.node_kind}
                    </p>
                    <p className="mt-0.5 text-[11.5px] leading-snug text-ink-faint">
                      {failure.attempts} attempt{failure.attempts === 1 ? "" : "s"}
                      {failure.last_error && ` — ${failure.last_error}`}
                    </p>
                  </li>
                ))}
              </ul>
            </section>
          )}
        </div>
      </div>
    </div>
  );
}

function ConnectorCard({
  connector,
  onChanged,
}: {
  connector: AvailableConnector;
  onChanged: () => Promise<void>;
}) {
  const [target, setTarget] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /**
   * The webhook's signing secret, shown exactly once.
   *
   * The engine cannot show it again — it lives in the keychain — so this is the only moment
   * it can be copied. Saying so is the difference between a user copying it and a user
   * reconnecting later to find out why their receiver rejects everything.
   */
  const [secret, setSecret] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const connect = async () => {
    if (!target.trim()) return;
    setBusy(true);
    setError(null);
    try {
      const result = await api.connectConnector(connector.id, target.trim());
      setSecret(result.signing_secret);
      setTarget("");
      await onChanged();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not connect that.");
    } finally {
      setBusy(false);
    }
  };

  const disconnect = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.disconnectConnector(connector.id);
      setSecret(null);
      await onChanged();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not disconnect that.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="card overflow-hidden">
      <div className="flex items-start gap-3 px-4 py-3">
        <span className="mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-overlay text-ink-muted">
          {connector.is_local ? <HardDrive size={15} aria-hidden /> : <Globe size={15} aria-hidden />}
        </span>

        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <h3 className="text-[13.5px] font-medium text-ink">{connector.display_name}</h3>
            {connector.connected && (
              <span className="flex items-center gap-1 rounded-full bg-ok px-2 py-0.5 text-[10.5px] font-medium text-ok-text">
                <Check size={9} strokeWidth={3} aria-hidden />
                Connected
              </span>
            )}
            <span
              className={`rounded-full px-2 py-0.5 text-[10.5px] ${
                connector.is_local ? "text-ink-faint" : "bg-warn text-warn-text"
              }`}
            >
              {connector.is_local ? "stays on this machine" : "leaves this machine"}
            </span>
          </div>

          <p className="mt-1 text-[12.5px] leading-relaxed text-ink-muted">
            {connector.description}
          </p>
        </div>

        {connector.connected && (
          <button
            type="button"
            onClick={() => void disconnect()}
            disabled={busy}
            className="shrink-0 rounded-full border border-hairline px-2.5 py-1 text-[12px]
                       text-ink-muted transition hover:bg-overlay hover:text-ink disabled:opacity-50"
          >
            Disconnect
          </button>
        )}
      </div>

      <div className="flex items-center gap-2 border-t border-hairline bg-overlay px-4 py-2.5">
        <label className="sr-only" htmlFor={`target-${connector.id}`}>
          {connector.target_label} for {connector.display_name}
        </label>
        <input
          id={`target-${connector.id}`}
          value={target}
          onChange={(event) => setTarget(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") void connect();
          }}
          placeholder={connector.target_hint}
          className="min-w-0 flex-1 rounded-lg border border-hairline bg-surface px-2.5 py-1.5
                     text-[12.5px] text-ink outline-none transition
                     placeholder:text-ink-faint focus:border-accent"
        />
        <button
          type="button"
          onClick={() => void connect()}
          disabled={busy || target.trim().length === 0}
          className="btn-accent shrink-0 py-1.5"
        >
          {busy ? (
            <Loader2 size={13} className="animate-spin" aria-hidden />
          ) : connector.connected ? (
            "Update"
          ) : (
            "Connect"
          )}
        </button>
      </div>

      {secret && (
        <div className="border-t border-warn-line bg-warn px-4 py-2.5">
          <p className="text-[11.5px] font-medium text-warn-text">
            Signing secret — copy it now, it cannot be shown again
          </p>
          <div className="mt-1 flex items-center gap-2">
            <code className="min-w-0 flex-1 truncate rounded bg-surface px-2 py-1 font-mono text-[11.5px] text-ink">
              {secret}
            </code>
            <button
              type="button"
              onClick={() => {
                void navigator.clipboard.writeText(secret);
                setCopied(true);
              }}
              className="flex shrink-0 items-center gap-1 rounded-full border border-warn-line
                         px-2 py-1 text-[11.5px] text-warn-text transition hover:bg-surface"
            >
              {copied ? <Check size={11} aria-hidden /> : <Copy size={11} aria-hidden />}
              {copied ? "Copied" : "Copy"}
            </button>
          </div>
        </div>
      )}

      {error && (
        <p role="alert" className="border-t border-hairline px-4 py-2 text-[12px] text-danger-text">
          {error}
        </p>
      )}
    </div>
  );
}
