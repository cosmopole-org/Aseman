#!/usr/bin/env python3
"""Generate dependency-free TypeScript and Python clients from A701 OpenAPI."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OPENAPI = ROOT / "contracts/public/openapi.json"
POLICY = ROOT / "contracts/public/client-policy.json"

TS_STREAM_TYPES = r'''
export interface EventStreamOptions extends RequestOptions {
  after?: number;
  maxEvents?: number;
}

export interface AsemanEvent {
  id: string | null;
  event: string;
  data: JsonValue;
}

function decodeEvent(block: string): AsemanEvent | null {
  let id: string | null = null;
  let event = "message";
  const data: string[] = [];
  for (const line of block.split("\n")) {
    if (line.startsWith(":")) continue;
    const colon = line.indexOf(":");
    const field = colon < 0 ? line : line.slice(0, colon);
    const value = colon < 0 ? "" : line.slice(colon + 1).replace(/^ /, "");
    if (field === "id") id = value;
    if (field === "event") event = value;
    if (field === "data") data.push(value);
  }
  if (data.length === 0) return null;
  const raw = data.join("\n");
  let decoded: JsonValue = raw;
  try { decoded = JSON.parse(raw) as JsonValue; } catch { /* data may be text */ }
  return { id, event, data: decoded };
}
'''

TS_STREAM_METHOD = r'''
  /** Subscribe to a creature-scoped A707 stream with Last-Event-ID replay. */
  async *events(
    stream: string,
    token: string,
    options: EventStreamOptions = {},
  ): AsyncGenerator<AsemanEvent> {
    const merged = { ...this.defaults, ...options };
    if (Boolean(merged.session) === Boolean(merged.proof)) {
      throw new TypeError("exactly one of session or proof is required");
    }
    const maxEvents = options.maxEvents ?? 100;
    if (!stream || !token) throw new TypeError("stream and token are required");
    if (maxEvents < 1 || maxEvents > 1000) {
      throw new TypeError("maxEvents must be between 1 and 1000");
    }
    const url = new URL(`/v1/events/${encodeURIComponent(stream)}`, this.baseUrl);
    url.searchParams.set("maxEvents", String(maxEvents));
    const headers: Record<string, string> = {
      "accept": "text/event-stream",
      "Aseman-Bridge-Token": token,
    };
    if (merged.session) headers["Aseman-Session"] = merged.session;
    if (merged.proof) headers["Aseman-Proof"] = merged.proof;
    if (merged.requestId) headers["Aseman-Request-Id"] = merged.requestId;
    if (options.after !== undefined) headers["Last-Event-ID"] = String(options.after);
    const response = await fetch(url, { headers, signal: merged.signal });
    if (!response.ok) {
      const raw = await response.text();
      let problem: JsonValue = raw;
      try { problem = JSON.parse(raw) as JsonValue; } catch { /* RFC problem may be unavailable */ }
      throw new AsemanApiError(response.status, problem, response.headers.get("Aseman-Request-Id"));
    }
    if (!response.body) throw new TypeError("event stream has no response body");
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffered = "";
    while (true) {
      const { done, value } = await reader.read();
      buffered += decoder.decode(value, { stream: !done }).replace(/\r\n/g, "\n");
      let boundary = buffered.indexOf("\n\n");
      while (boundary >= 0) {
        const decoded = decodeEvent(buffered.slice(0, boundary));
        buffered = buffered.slice(boundary + 2);
        if (decoded) yield decoded;
        boundary = buffered.indexOf("\n\n");
      }
      if (done) break;
    }
    const finalEvent = decodeEvent(buffered);
    if (finalEvent) yield finalEvent;
  }
'''

PY_STREAM_METHOD = r'''
    def events(
        self,
        stream: str,
        token: str,
        *,
        after: int | None = None,
        max_events: int = 100,
        **options: str,
    ) -> Iterator[JsonObject]:
        """Subscribe to a creature-scoped A707 stream with Last-Event-ID replay."""
        session = options.get("session", self.session)
        proof = options.get("proof", self.proof)
        if bool(session) == bool(proof):
            raise TypeError("exactly one of session or proof is required")
        if not stream or not token:
            raise TypeError("stream and token are required")
        if not 1 <= max_events <= 1000:
            raise TypeError("max_events must be between 1 and 1000")
        query = urllib.parse.urlencode({"maxEvents": max_events})
        path = "/v1/events/" + urllib.parse.quote(stream, safe="") + "?" + query
        headers = {"Accept": "text/event-stream", "Aseman-Bridge-Token": token}
        headers["Aseman-Session" if session else "Aseman-Proof"] = session or proof or ""
        if options.get("request_id"):
            headers["Aseman-Request-Id"] = options["request_id"]
        if after is not None:
            headers["Last-Event-ID"] = str(after)
        request = urllib.request.Request(self.base_url + path, headers=headers, method="GET")
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                event_id: str | None = None
                event_type = "message"
                data: list[str] = []
                for raw_line in response:
                    line = raw_line.decode("utf-8").rstrip("\r\n")
                    if not line:
                        if data:
                            joined = "\n".join(data)
                            try:
                                decoded: JsonValue = json.loads(joined)
                            except json.JSONDecodeError:
                                decoded = joined
                            yield {"id": event_id, "event": event_type, "data": decoded}
                        event_id, event_type, data = None, "message", []
                        continue
                    if line.startswith(":"):
                        continue
                    field, separator, value = line.partition(":")
                    if separator and value.startswith(" "):
                        value = value[1:]
                    if field == "id":
                        event_id = value
                    elif field == "event":
                        event_type = value
                    elif field == "data":
                        data.append(value)
                if data:
                    joined = "\n".join(data)
                    try:
                        decoded = json.loads(joined)
                    except json.JSONDecodeError:
                        decoded = joined
                    yield {"id": event_id, "event": event_type, "data": decoded}
        except urllib.error.HTTPError as error:
            raw = error.read()
            try:
                problem: JsonValue = json.loads(raw) if raw else None
            except json.JSONDecodeError:
                problem = raw.decode(errors="replace")
            raise AsemanApiError(error.code, problem, error.headers.get("Aseman-Request-Id")) from error
'''


def operations() -> tuple[str, list[dict[str, object]]]:
    document = json.loads(OPENAPI.read_text())
    version = str(document["info"]["version"])
    found: list[dict[str, object]] = []
    for path, path_item in sorted(document["paths"].items()):
        for method, operation in sorted(path_item.items()):
            found.append(
                {
                    "name": operation["operationId"],
                    "path": path,
                    "method": method.upper(),
                    "mutation": operation["x-aseman-class"] != "read",
                    "action": operation["x-aseman-action"],
                    "summary": operation["summary"],
                }
            )
    names = [str(item["name"]) for item in found]
    if len(names) != len(set(names)):
        raise SystemExit("public client generation: duplicate operationId")
    return version, found


def snake(name: str) -> str:
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


def typescript(version: str, items: list[dict[str, object]]) -> str:
    methods = []
    catalogue = []
    for item in items:
        name = item["name"]
        path = item["path"]
        mutation = "true" if item["mutation"] else "false"
        summary = str(item["summary"]).replace("*/", "* /")
        action = item["action"]
        methods.append(
            f'''  /** {summary}. Policy action: `{action}`. */
  {name}(body: JsonObject = {{}}, options: RequestOptions = {{}}): Promise<JsonValue> {{
    return this.invoke("{path}", {mutation}, body, options);
  }}'''
        )
        catalogue.append(f'  {name}: {{ path: "{path}", mutation: {mutation} }},')
    return f'''// @generated by scripts/generate_public_clients.py from contracts/public/openapi.json.
// Do not edit. Public API major: {version}.

export type JsonPrimitive = string | number | boolean | null;
export type JsonValue = JsonPrimitive | JsonObject | JsonValue[];
export interface JsonObject {{ [key: string]: JsonValue; }}

export interface RequestOptions {{
  session?: string;
  proof?: string;
  requestId?: string;
  idempotencyKey?: string;
  signal?: AbortSignal;
}}

{TS_STREAM_TYPES}

export class AsemanApiError extends Error {{
  constructor(
    public readonly status: number,
    public readonly problem: JsonValue,
    public readonly requestId: string | null,
  ) {{
    super(`Aseman API request failed with HTTP ${{status}}`);
    this.name = "AsemanApiError";
  }}
}}

export const operations = {{
{chr(10).join(catalogue)}
}} as const;

export class AsemanClient {{
  constructor(
    private readonly baseUrl: string,
    private readonly defaults: RequestOptions = {{}},
  ) {{}}

  private async invoke(
    path: string,
    mutation: boolean,
    body: JsonObject,
    options: RequestOptions,
  ): Promise<JsonValue> {{
    const merged = {{ ...this.defaults, ...options }};
    if (Boolean(merged.session) === Boolean(merged.proof)) {{
      throw new TypeError("exactly one of session or proof is required");
    }}
    if (mutation && !merged.idempotencyKey) {{
      throw new TypeError("idempotencyKey is required for mutating operations");
    }}
    const headers: Record<string, string> = {{
      "accept": "application/json",
      "content-type": "application/json",
    }};
    if (merged.session) headers["Aseman-Session"] = merged.session;
    if (merged.proof) headers["Aseman-Proof"] = merged.proof;
    if (merged.requestId) headers["Aseman-Request-Id"] = merged.requestId;
    if (merged.idempotencyKey) headers["Idempotency-Key"] = merged.idempotencyKey;
    const response = await fetch(new URL(path, this.baseUrl), {{
      method: "POST",
      headers,
      body: JSON.stringify(body),
      signal: merged.signal,
    }});
    const raw = await response.text();
    let decoded: JsonValue = null;
    if (raw) {{
      try {{ decoded = JSON.parse(raw) as JsonValue; }}
      catch {{ decoded = raw; }}
    }}
    if (!response.ok) {{
      throw new AsemanApiError(response.status, decoded, response.headers.get("Aseman-Request-Id"));
    }}
    return decoded;
  }}

{chr(10).join(methods)}
{TS_STREAM_METHOD}
}}
'''


def python(version: str, items: list[dict[str, object]]) -> str:
    methods = []
    catalogue = []
    for item in items:
        py_name = snake(str(item["name"]))
        path = item["path"]
        mutation = "True" if item["mutation"] else "False"
        summary = repr(f"{item['summary']}. Policy action: {item['action']}.")
        methods.append(
            f'''    def {py_name}(self, body: JsonObject | None = None, **options: str) -> JsonValue:
        {summary}
        return self._invoke("{path}", {mutation}, body or {{}}, **options)
'''
        )
        catalogue.append(f'    "{py_name}": {{"path": "{path}", "mutation": {mutation}}},')
    return f'''# @generated by scripts/generate_public_clients.py from contracts/public/openapi.json.
# Do not edit. Public API major: {version}.
from __future__ import annotations

import json
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, Iterator, TypeAlias

JsonValue: TypeAlias = Any
JsonObject: TypeAlias = dict[str, JsonValue]

OPERATIONS = {{
{chr(10).join(catalogue)}
}}


class AsemanApiError(RuntimeError):
    def __init__(self, status: int, problem: JsonValue, request_id: str | None) -> None:
        super().__init__(f"Aseman API request failed with HTTP {{status}}")
        self.status = status
        self.problem = problem
        self.request_id = request_id


class AsemanClient:
    def __init__(self, base_url: str, *, session: str | None = None, proof: str | None = None, timeout: float = 30.0) -> None:
        self.base_url = base_url.rstrip("/")
        self.session = session
        self.proof = proof
        self.timeout = timeout

    def _invoke(self, path: str, mutation: bool, body: JsonObject, **options: str) -> JsonValue:
        session = options.get("session", self.session)
        proof = options.get("proof", self.proof)
        if bool(session) == bool(proof):
            raise TypeError("exactly one of session or proof is required")
        idempotency_key = options.get("idempotency_key")
        if mutation and not idempotency_key:
            raise TypeError("idempotency_key is required for mutating operations")
        headers = {{"Accept": "application/json", "Content-Type": "application/json"}}
        headers["Aseman-Session" if session else "Aseman-Proof"] = session or proof or ""
        if options.get("request_id"):
            headers["Aseman-Request-Id"] = options["request_id"]
        if idempotency_key:
            headers["Idempotency-Key"] = idempotency_key
        request = urllib.request.Request(
            self.base_url + path,
            data=json.dumps(body, separators=(",", ":")).encode(),
            headers=headers,
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                raw = response.read()
                return json.loads(raw) if raw else None
        except urllib.error.HTTPError as error:
            raw = error.read()
            try:
                problem: JsonValue = json.loads(raw) if raw else None
            except json.JSONDecodeError:
                problem = raw.decode(errors="replace")
            raise AsemanApiError(error.code, problem, error.headers.get("Aseman-Request-Id")) from error

{chr(10).join(methods)}
{PY_STREAM_METHOD}
'''


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    policy = json.loads(POLICY.read_text())
    version, items = operations()
    renderers = {"typescript": typescript(version, items), "python": python(version, items)}
    errors = []
    for target in policy["generated"]:
        output = ROOT / target["path"]
        rendered = renderers[target["language"]]
        if args.check:
            if not output.exists() or output.read_text() != rendered:
                errors.append(str(output.relative_to(ROOT)))
        else:
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_text(rendered)
    if errors:
        raise SystemExit("generated public clients are stale: " + ", ".join(errors))
    print(f"public clients are up to date ({len(items)} operations, {len(renderers)} languages)")


if __name__ == "__main__":
    main()
