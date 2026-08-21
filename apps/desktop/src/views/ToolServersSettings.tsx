import { useCallback, useEffect, useState } from "react";
import {
  AlertTriangle,
  ChevronDown,
  Loader2,
  Play,
  Plug,
  Power,
  Square,
  Trash2,
} from "lucide-react";

import { api, ApiError, type McpDiscovery, type McpServerInfo } from "../lib/api";
import { parseSecrets } from "../lib/secrets";

/**
 * External tool servers, and which of their tools may be proposed.
 *
 * # Why adding a server grants nothing
 *
 * A configured server is not a usable one. It arrives off, with none of its tools allowed, and both
 * have to be turned on by hand. That is the same rule the MCP *server* applies to its own write
 * scope, generalised: connecting a client should not grant capability as a side effect of having
 * typed a command in.
 *
 * # Why the trust warning is on the screen and not in a document
 *
 * A tool call carries whatever arguments the proposal contains, which may include what was said in
 * a meeting. Whether that is acceptable is a judgement about the server's operator, and this is
 * where that judgement gets made — so it is where it has to be said.
 *
 * # Why there is no "always allow"
 *
 * Every call is confirmed individually, every time. A remembered per-tool allow would become
 * auto-execute within a week of use, and an unattended path into other people's systems is the one
 * thing this product does not have anywhere by design.
 */
export function ToolServersSettings() {
  const [servers, setServers] = useState<McpServerInfo[]>([]);
  const [tools, setTools] = useState<Record<string, McpDiscovery>>({});
  const [expanded, setExpanded] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);

  const load = useCallback(async () => {
    try {
      setServers(await api.mcpServers());
    } catch {
      // A server list that will not load is not worth a banner over the whole settings screen.
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  /** Ask a server what it publishes. Starts it, unless it is pinned off. */
  const discover = useCallback(async (id: string) => {
    setBusy(id);
    try {
      const found = await api.mcpServerTools(id);
      setTools((current) => ({ ...current, [id]: found }));
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not reach that server.");
    } finally {
      setBusy(null);
    }
  }, []);

  const expand = async (id: string) => {
    if (expanded === id) {
      setExpanded(null);
      return;
    }
    setExpanded(id);
    if (!tools[id]) await discover(id);
  };

  const act = async (id: string, work: () => Promise<unknown>) => {
    setBusy(id);
    setError(null);
    try {
      await work();
      await load();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "That did not work.");
    } finally {
      setBusy(null);
    }
  };

  const toggleTool = async (server: McpServerInfo, tool: string, on: boolean) => {
    await act(server.id, () =>
      on ? api.enableMcpTool(server.id, tool) : api.disableMcpTool(server.id, tool),
    );
  };

  return (
    <section>
      <h2 className="mb-1 flex items-center gap-1.5 text-[13px] font-semibold text-ink">
        <Plug size={13} className="text-ink-faint" aria-hidden />
        External tools
      </h2>
      <p className="mb-3 text-[12px] leading-relaxed text-ink-muted">
        Connect MCP servers so an action item can become a ticket, a message, or whatever else they
        offer. Every call is shown to you and confirmed before it runs — there is no automatic
        execution and no way to turn the confirmation off.
      </p>

      {error && (
        <div
          role="alert"
          className="mb-3 rounded-lg border border-warn-line bg-warn px-3 py-2 text-[12.5px] text-warn-text"
        >
          {error}
        </div>
      )}

      {servers.length > 0 && (
        <ul className="mb-3 divide-y divide-hairline overflow-hidden rounded-lg border border-hairline">
          {servers.map((server) => {
            const found = tools[server.id];
            const open = expanded === server.id;

            return (
              <li key={server.id}>
                <div className="flex items-center gap-3 px-3 py-2.5">
                  <button
                    type="button"
                    onClick={() => void expand(server.id)}
                    className="flex min-w-0 flex-1 items-center gap-2 text-left"
                    aria-expanded={open}
                  >
                    <ChevronDown
                      size={13}
                      className={`shrink-0 text-ink-faint transition ${open ? "" : "-rotate-90"}`}
                      aria-hidden
                    />
                    <span className="min-w-0">
                      <span className="block truncate text-[13px] text-ink">{server.name}</span>
                      <span className="block truncate text-[11.5px] text-ink-faint">
                        {server.transport === "stdio" ? server.command : server.url}
                        {server.enabled_tools.length > 0 &&
                          ` · ${server.enabled_tools.length} tool${
                            server.enabled_tools.length === 1 ? "" : "s"
                          } allowed`}
                        {server.running && " · running"}
                        {!server.auto_start && " · manual start"}
                      </span>
                    </span>
                  </button>

                  <button
                    type="button"
                    onClick={() =>
                      void act(server.id, () =>
                        api.setMcpServerEnabled(server.id, !server.enabled),
                      )
                    }
                    title={server.enabled ? "Turn this server off" : "Turn this server on"}
                    className={`flex shrink-0 items-center gap-1 rounded-full px-2.5 py-1 text-[11.5px]
                                transition ${
                                  server.enabled
                                    ? "bg-accent text-accent-on hover:opacity-90"
                                    : "border border-hairline text-ink-muted hover:text-ink"
                                }`}
                  >
                    {busy === server.id ? (
                      <Loader2 size={11} className="animate-spin" aria-hidden />
                    ) : (
                      <Power size={11} aria-hidden />
                    )}
                    {server.enabled ? "On" : "Off"}
                  </button>

                  {server.running ? (
                    <button
                      type="button"
                      onClick={() => void act(server.id, () => api.stopMcpServer(server.id))}
                      title="Stop the process"
                      className="shrink-0 rounded p-1 text-ink-faint transition hover:text-ink"
                    >
                      <Square size={12} aria-hidden />
                    </button>
                  ) : (
                    <button
                      type="button"
                      onClick={() =>
                        void act(server.id, async () => {
                          const found = await api.startMcpServer(server.id);
                          setTools((current) => ({ ...current, [server.id]: found }));
                          setExpanded(server.id);
                        })
                      }
                      title="Start the process"
                      className="shrink-0 rounded p-1 text-ink-faint transition hover:text-ink"
                    >
                      <Play size={12} aria-hidden />
                    </button>
                  )}

                  <button
                    type="button"
                    onClick={() => void act(server.id, () => api.deleteMcpServer(server.id))}
                    title="Remove this server"
                    className="shrink-0 rounded p-1 text-ink-faint transition hover:text-warn-text"
                  >
                    <Trash2 size={12} aria-hidden />
                  </button>
                </div>

                {open && (
                  <div className="border-t border-hairline bg-overlay px-3 py-2.5">
                    {!found ? (
                      <p className="flex items-center gap-2 text-[12px] text-ink-faint">
                        <Loader2 size={12} className="animate-spin" aria-hidden />
                        Asking {server.name} what it can do
                      </p>
                    ) : found.error ? (
                      <p className="flex items-start gap-1.5 text-[12px] leading-relaxed text-warn-text">
                        <AlertTriangle size={12} className="mt-0.5 shrink-0" aria-hidden />
                        {found.error}
                      </p>
                    ) : found.tools.length === 0 ? (
                      <p className="text-[12px] text-ink-faint">
                        This server publishes no tools.
                      </p>
                    ) : (
                      <>
                        <p className="mb-2 text-[11.5px] text-ink-faint">
                          Tick a tool to let it be proposed. Untick to withdraw it. Nothing runs
                          without your confirmation either way.
                        </p>
                        <ul className="space-y-1.5">
                          {found.tools.map((tool) => (
                            <li key={tool.name} className="flex items-start gap-2">
                              <input
                                id={`${server.id}-${tool.name}`}
                                type="checkbox"
                                checked={server.enabled_tools.includes(tool.name)}
                                onChange={(event) =>
                                  void toggleTool(server, tool.name, event.target.checked)
                                }
                                className="mt-0.5 h-3.5 w-3.5 accent-[var(--accent)]"
                              />
                              <label
                                htmlFor={`${server.id}-${tool.name}`}
                                className="min-w-0 cursor-pointer"
                              >
                                <code className="text-[12px] text-ink">{tool.name}</code>
                                {tool.description && (
                                  <span className="ml-1.5 text-[11.5px] text-ink-faint">
                                    {tool.description}
                                  </span>
                                )}
                              </label>
                            </li>
                          ))}
                        </ul>
                      </>
                    )}

                    {!server.enabled && (
                      <p className="mt-2 text-[11.5px] text-ink-faint">
                        This server is off, so none of its tools can be proposed yet — your
                        choices here are kept for when you turn it on.
                      </p>
                    )}
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}

      {adding ? (
        <AddServerForm
          onCancel={() => setAdding(false)}
          onAdded={async () => {
            setAdding(false);
            await load();
          }}
        />
      ) : (
        <button
          type="button"
          onClick={() => setAdding(true)}
          className="rounded-lg border border-hairline px-3 py-1.5 text-[12.5px] text-ink-muted
                     transition hover:bg-overlay hover:text-ink"
        >
          Add a server
        </button>
      )}
    </section>
  );
}

/**
 * The add form.
 *
 * Says what a server sees, because whether that is acceptable is the user's judgement to make and
 * this is the moment they make it.
 */
function AddServerForm({
  onCancel,
  onAdded,
}: {
  onCancel: () => void;
  onAdded: () => Promise<void>;
}) {
  const [name, setName] = useState("");
  const [transport, setTransport] = useState<"stdio" | "http">("stdio");
  const [command, setCommand] = useState("");
  const [args, setArgs] = useState("");
  const [url, setUrl] = useState("");
  const [secrets, setSecrets] = useState("");
  const [autoStart, setAutoStart] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async () => {
    setSaving(true);
    setError(null);
    try {
      await api.addMcpServer({
        name: name.trim(),
        transport,
        command: transport === "stdio" ? command.trim() : undefined,
        // Split on whitespace, which is what a command line looks like. A server needing an
        // argument with a space in it is rare enough to be worth an API call rather than a
        // quoting syntax nobody can remember.
        args: transport === "stdio" ? args.split(/\s+/).filter(Boolean) : undefined,
        url: transport === "http" ? url.trim() : undefined,
        auto_start: autoStart,
        secrets: parseSecrets(secrets),
      });
      await onAdded();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "Could not add that server.");
    } finally {
      setSaving(false);
    }
  };

  const complete = name.trim() && (transport === "stdio" ? command.trim() : url.trim());

  return (
    <div className="card space-y-2.5 p-3">
      <div className="flex gap-2">
        <input
          value={name}
          onChange={(event) => setName(event.target.value)}
          placeholder="Name, e.g. linear"
          className="min-w-0 flex-1 rounded border border-hairline bg-transparent px-2 py-1
                     text-[12.5px] text-ink placeholder:text-ink-faint"
        />
        <select
          value={transport}
          onChange={(event) => setTransport(event.target.value as "stdio" | "http")}
          className="rounded border border-hairline bg-transparent px-2 py-1 text-[12.5px] text-ink"
        >
          <option value="stdio">Local command</option>
          <option value="http">Remote URL</option>
        </select>
      </div>

      {transport === "stdio" ? (
        <div className="flex gap-2">
          <input
            value={command}
            onChange={(event) => setCommand(event.target.value)}
            placeholder="Command, e.g. npx"
            className="min-w-0 flex-1 rounded border border-hairline bg-transparent px-2 py-1
                       font-mono text-[12px] text-ink placeholder:text-ink-faint"
          />
          <input
            value={args}
            onChange={(event) => setArgs(event.target.value)}
            placeholder="Arguments, space separated"
            className="min-w-0 flex-[2] rounded border border-hairline bg-transparent px-2 py-1
                       font-mono text-[12px] text-ink placeholder:text-ink-faint"
          />
        </div>
      ) : (
        <input
          value={url}
          onChange={(event) => setUrl(event.target.value)}
          placeholder="https://example.com/mcp"
          className="w-full rounded border border-hairline bg-transparent px-2 py-1 font-mono
                     text-[12px] text-ink placeholder:text-ink-faint"
        />
      )}

      <textarea
        value={secrets}
        onChange={(event) => setSecrets(event.target.value)}
        rows={2}
        placeholder={
          transport === "stdio"
            ? "Environment, one KEY=value per line (optional)"
            : "Headers, one Name: value per line (optional)"
        }
        className="w-full rounded border border-hairline bg-transparent px-2 py-1 font-mono
                   text-[12px] text-ink placeholder:text-ink-faint"
      />
      <p className="text-[11px] text-ink-faint">
        Anything here goes to your system keychain, not the workspace database.
      </p>

      <label className="flex cursor-pointer items-center gap-1.5 text-[12px] text-ink-muted">
        <input
          type="checkbox"
          checked={autoStart}
          onChange={(event) => setAutoStart(event.target.checked)}
          className="h-3.5 w-3.5 accent-[var(--accent)]"
        />
        Start it automatically when a tool is needed
      </label>

      <p className="rounded border border-hairline bg-overlay px-2 py-1.5 text-[11.5px] leading-relaxed text-ink-muted">
        A server you connect sees the arguments of any call you approve, which can include what was
        said in a meeting. Add servers you would trust with that.
      </p>

      {error && <p className="text-[12px] text-warn-text">{error}</p>}

      <div className="flex justify-end gap-2">
        <button
          type="button"
          onClick={onCancel}
          className="rounded-full border border-hairline px-2.5 py-1 text-[11.5px] text-ink-muted
                     transition hover:text-ink"
        >
          Cancel
        </button>
        <button
          type="button"
          onClick={() => void submit()}
          disabled={saving || !complete}
          className="flex items-center gap-1 rounded-full bg-accent px-2.5 py-1 text-[11.5px]
                     text-accent-on transition hover:opacity-90 disabled:opacity-50"
        >
          {saving && <Loader2 size={11} className="animate-spin" aria-hidden />}
          Add
        </button>
      </div>
    </div>
  );
}
