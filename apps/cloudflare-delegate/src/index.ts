// A call-mode delegate on a Cloudflare Worker (`surface.delegate`): the daemon posts one
// signed envelope per item attempt, the handler answers in the outcome vocabulary, and the
// answer goes back signed over the request's idempotency key. The engine's clauses are
// normative; every restatement here names its clause with a `mirrors:` comment.

/** The entity an isolated item runs under, with the fence its lease holds. */
export type Entity = { namespace: string; key: string; fence: number };

// mirrors: surface.delegate.envelope
/** What one delegate request carries. */
export type Envelope = {
  job: string;
  execution_id: string;
  step: string;
  input_hash: string;
  attempt: number;
  idempotency_key: string;
  entity?: Entity;
  as_of: string;
  /** Present only when the job fires from an applied snapshot. */
  version?: number;
  pins: Record<string, string>;
  tables: string[];
  row: Record<string, unknown>;
  /** The engine's instant at send plus the binding's timeout, RFC 3339 UTC. */
  deadline: string;
};

/** One row of one output table, as the engine lands it: a plain object of columns. */
export type Row = Record<string, unknown>;

// mirrors: surface.delegate.outcome
/** A delegate's answer. */
export type Answer =
  | { outcome: "ok"; rows: Record<string, Row[]> }
  | { outcome: "defer"; retry_at: string; reason?: string }
  | { outcome: "transient"; reason?: string }
  | { outcome: "permanent"; reason?: string };

/**
 * An RFC 3339 instant as the engine reads it: date, `T`, time, optional fraction, then `Z`
 * or an offset. The package re-emits it in `Z` form.
 */
const INSTANT = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,9})?(Z|[+-]\d{2}:\d{2})$/;

/** `text` as the engine's `Z` spelling, or `null` when it is no instant. */
function instant(text: string): string | null {
  if (!INSTANT.test(text)) return null;
  const at = Date.parse(text);
  if (Number.isNaN(at)) return null;
  return new Date(at).toISOString().replace(/\.000Z$/, "Z");
}

export const ok = (rows: Record<string, Row[]>): Answer => ({ outcome: "ok", rows });

/**
 * Park the item until `retryAt`. An invalid `Date` or an unreadable string is a handler
 * bug and raises `DelegateAnswerInvalid`, which the fetch handler answers as a signed
 * `permanent`.
 */
export const defer = (retryAt: Date | string, reason?: string): Answer => {
  const text = typeof retryAt === "string" ? retryAt : Number.isNaN(retryAt.getTime()) ? "invalid Date" : retryAt.toISOString();
  const at = instant(text);
  if (at === null) throw new DelegateAnswerInvalid(`\`retry_at\` \`${text}\` is no RFC 3339 instant`);
  return { outcome: "defer", retry_at: at, ...(reason === undefined ? {} : { reason }) };
};
export const transient = (reason?: string): Answer => ({ outcome: "transient", ...(reason === undefined ? {} : { reason }) });
export const permanent = (reason?: string): Answer => ({ outcome: "permanent", ...(reason === undefined ? {} : { reason }) });

/** The header names the request and the answer carry. */
export const TIMESTAMP_HEADER = "x-contextful-timestamp";
export const SIGNATURE_HEADER = "x-contextful-signature";

// mirrors: surface.dispatch.callback-skew
/** Seconds a signature timestamp may differ from this side's system clock. */
export const SKEW_SECS = 300;

// mirrors: surface.dispatch.step-result
/** Largest answer body the engine admits, in bytes. */
export const ANSWER_CAP = 1024 * 1024;

/** Longest fault text a fallback `permanent` carries, so it never passes the cap. */
const REASON_CAP = 512;

/** Why a request was refused before the handler ran. */
export class DelegateRequestRejected extends Error {
  readonly status: 401 | 422;
  constructor(status: 401 | 422, message: string) {
    super(message);
    this.name = "DelegateRequestRejected";
    this.status = status;
  }
}

/** Why an answer the handler produced cannot be sent: a deterministic handler bug. */
export class DelegateAnswerInvalid extends Error {
  constructor(message: string) {
    super(message);
    this.name = "DelegateAnswerInvalid";
  }
}

const encoder = new TextEncoder();

type Key = string | Uint8Array;

const importedStrings = new Map<string, Promise<CryptoKey>>();
const importedBytes = new WeakMap<Uint8Array, Promise<CryptoKey>>();

/** The HMAC key for `key`, imported once per key value; a failed import is not kept. */
function cryptoKey(key: Key): Promise<CryptoKey> {
  const raw = typeof key === "string" ? encoder.encode(key) : key;
  if (raw.length === 0) throw new TypeError("the deployment key is empty");
  const cached = typeof key === "string" ? importedStrings.get(key) : importedBytes.get(key);
  if (cached !== undefined) return cached;
  const pending = crypto.subtle.importKey("raw", raw as BufferSource, { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  if (typeof key === "string") {
    importedStrings.set(key, pending);
    pending.catch(() => importedStrings.delete(key));
  } else {
    importedBytes.set(key, pending);
    pending.catch(() => importedBytes.delete(key));
  }
  return pending;
}

async function hmac(key: Key, message: Uint8Array): Promise<Uint8Array> {
  return new Uint8Array(await crypto.subtle.sign("HMAC", await cryptoKey(key), message as BufferSource));
}

const hex = (bytes: Uint8Array): string => Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");

function unhex(text: string): Uint8Array | null {
  if (text.length % 2 !== 0 || !/^[0-9a-f]*$/i.test(text)) return null;
  const out = new Uint8Array(text.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(text.slice(2 * i, 2 * i + 2), 16);
  return out;
}

function concat(prefix: string, body: Uint8Array): Uint8Array {
  const head = encoder.encode(prefix);
  const out = new Uint8Array(head.length + body.length);
  out.set(head, 0);
  out.set(body, head.length);
  return out;
}

/** Whether two byte strings are equal, in time independent of where they differ. */
function timingSafeEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  const subtle = crypto.subtle as SubtleCrypto & { timingSafeEqual?: (a: BufferSource, b: BufferSource) => boolean };
  if (typeof subtle.timingSafeEqual === "function") return subtle.timingSafeEqual(a as BufferSource, b as BufferSource);
  let diff = 0;
  for (let i = 0; i < a.length; i += 1) diff |= a[i] ^ b[i];
  return diff === 0;
}

// mirrors: surface.delegate.signed
/** The request signature: HMAC-SHA256 under `key` over `call\n<timestamp>\n` and the body, hex. */
export async function signRequest(key: Key, timestamp: number, body: Uint8Array): Promise<string> {
  return hex(await hmac(key, concat(`call\n${timestamp}\n`, body)));
}

// mirrors: surface.delegate.signed
/**
 * The answer signature: HMAC-SHA256 under `key` over `answer\n<idempotency_key>\n<timestamp>\n`
 * and the body, hex, so an answer verifies only for the request it answers.
 */
export async function signAnswer(key: Key, idempotencyKey: string, timestamp: number, body: Uint8Array): Promise<string> {
  return hex(await hmac(key, concat(`answer\n${idempotencyKey}\n${timestamp}\n`, body)));
}

/** Seconds since the Unix epoch on this side's system clock (`surface.delegate.clocks`). */
export const systemNow = (): number => Math.floor(Date.now() / 1000);

/** The largest delay a JavaScript timer takes, in milliseconds. */
const TIMER_MAX_MS = 2 ** 31 - 1;

/**
 * Milliseconds from the daemon's send instant to the engine's `deadline`, clamped to
 * `[0, 2^31 - 1]` so a timer never overflows. The send instant is the request's signature
 * timestamp, this side's reading of the daemon's wall clock; pass the request, that
 * timestamp in Unix seconds, or an instant. Under an injected engine clock (`--now`) the
 * deadline sits on another clock than the timestamp, so the clamp is the only safe reading.
 */
export function timeToDeadline(envelope: Envelope, sentAt: Request | number | Date | string): number {
  const deadline = Date.parse(envelope.deadline);
  let from: number;
  if (sentAt instanceof Request) {
    const header = Number(sentAt.headers.get(TIMESTAMP_HEADER));
    from = Number.isInteger(header) ? header * 1000 : Number.NaN;
  } else if (typeof sentAt === "number") {
    from = sentAt * 1000;
  } else if (typeof sentAt === "string") {
    from = Date.parse(sentAt);
  } else {
    from = sentAt.getTime();
  }
  if (Number.isNaN(deadline) || Number.isNaN(from)) return 0;
  return Math.min(Math.max(deadline - from, 0), TIMER_MAX_MS);
}

/**
 * Admit one delegate request: signed under `key` over its timestamp and body, the
 * timestamp within the skew of this side's system clock, the body an envelope. Refuses with
 * 401 for a signature or skew failure and 422 for a body that is no envelope. Reads the
 * request's body; pass a clone when the caller reads it too.
 */
export async function verifyRequest(request: Request, key: Key, now: () => number = systemNow): Promise<Envelope> {
  const timestampHeader = request.headers.get(TIMESTAMP_HEADER);
  const signature = request.headers.get(SIGNATURE_HEADER);
  if (timestampHeader === null || signature === null) {
    throw new DelegateRequestRejected(401, `a request carries \`${TIMESTAMP_HEADER}\` and \`${SIGNATURE_HEADER}\``);
  }
  const timestamp = Number(timestampHeader);
  if (!Number.isInteger(timestamp)) {
    throw new DelegateRequestRejected(401, `\`${TIMESTAMP_HEADER}\` \`${timestampHeader}\` is no Unix second`);
  }
  const body = new Uint8Array(await request.arrayBuffer());
  const presented = unhex(signature);
  if (presented === null || !timingSafeEqual(await hmac(key, concat(`call\n${timestamp}\n`, body)), presented)) {
    throw new DelegateRequestRejected(401, "the request's signature does not verify under the deployment key");
  }
  const skew = Math.abs(now() - timestamp);
  if (skew > SKEW_SECS) {
    throw new DelegateRequestRejected(401, `the request's timestamp is ${skew} s from this clock, past ${SKEW_SECS} s`);
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(new TextDecoder().decode(body));
  } catch (error) {
    throw new DelegateRequestRejected(422, `the body is no envelope: ${describe(error)}`);
  }
  return envelopeOf(parsed);
}

/** A plain object: one created by a literal or `Object.create(null)`, no `Map`, `Date` or instance. */
function isPlain(value: unknown): value is Record<string, unknown> {
  if (typeof value !== "object" || value === null) return false;
  const proto = Object.getPrototypeOf(value);
  return proto === Object.prototype || proto === null;
}

// mirrors: surface.delegate.envelope
function envelopeOf(value: unknown): Envelope {
  const malformed = (what: string) => new DelegateRequestRejected(422, `the body is no envelope: ${what}`);
  if (!isPlain(value)) throw malformed("not an object");
  const e = value;
  for (const field of ["job", "execution_id", "step", "input_hash", "idempotency_key", "as_of", "deadline"]) {
    if (typeof e[field] !== "string") throw malformed(`\`${field}\` is a string`);
  }
  if (!Number.isInteger(e.attempt)) throw malformed("`attempt` is an integer");
  if (e.version !== undefined && !Number.isInteger(e.version)) throw malformed("`version` is an integer when present");
  if (!isPlain(e.pins) || !Object.values(e.pins).every((v) => typeof v === "string")) throw malformed("`pins` maps names to strings");
  if (!Array.isArray(e.tables) || !e.tables.every((t) => typeof t === "string")) throw malformed("`tables` is a list of names");
  if (!isPlain(e.row)) throw malformed("`row` is an object");
  if (e.entity !== undefined) {
    const entity = e.entity;
    if (!isPlain(entity) || typeof entity.namespace !== "string" || typeof entity.key !== "string" || !Number.isInteger(entity.fence)) {
      throw malformed("`entity` carries namespace, key and fence");
    }
  }
  return e as unknown as Envelope;
}

// mirrors: surface.delegate.outcome
/** The answer as it leaves: in the vocabulary, rows for declared tables, `retry_at` in `Z` form. */
function checked(envelope: Envelope, answer: Answer): Answer {
  if (!isPlain(answer)) throw new DelegateAnswerInvalid("an answer is an object");
  switch (answer.outcome) {
    case "ok":
      if (!isPlain(answer.rows)) throw new DelegateAnswerInvalid("`rows` maps each table to its rows");
      for (const [table, rows] of Object.entries(answer.rows)) {
        if (!envelope.tables.includes(table)) {
          throw new DelegateAnswerInvalid(`rows for \`${table}\`, which the job does not declare in \`tables\` (${envelope.tables.join(", ")})`);
        }
        if (!Array.isArray(rows) || !rows.every(isPlain)) throw new DelegateAnswerInvalid(`\`rows.${table}\` is a list of plain row objects`);
      }
      return answer;
    case "defer": {
      const at = typeof answer.retry_at === "string" ? instant(answer.retry_at) : null;
      if (at === null) throw new DelegateAnswerInvalid(`\`retry_at\` \`${String(answer.retry_at)}\` is no RFC 3339 instant`);
      return { ...answer, retry_at: at };
    }
    case "transient":
    case "permanent":
      return answer;
    default:
      throw new DelegateAnswerInvalid("outcome is `ok`, `defer`, `transient` or `permanent`");
  }
}

/**
 * The signed response carrying `answer` to the request `envelope` came in. Refuses an answer
 * outside the vocabulary, naming an undeclared table, malformed, or over the engine's cap,
 * before it leaves.
 */
export async function respond(envelope: Envelope, answer: Answer, key: Key, now: () => number = systemNow): Promise<Response> {
  const sent = checked(envelope, answer);
  let text: string;
  try {
    text = JSON.stringify(sent);
  } catch (error) {
    throw new DelegateAnswerInvalid(`the answer does not serialize: ${describe(error)}`);
  }
  const body = encoder.encode(text);
  if (body.length > ANSWER_CAP) {
    throw new DelegateAnswerInvalid(`an answer of ${body.length} B exceeds the ${ANSWER_CAP} B cap`);
  }
  const timestamp = Math.floor(now());
  return new Response(body, {
    status: 200,
    headers: {
      "content-type": "application/json",
      [TIMESTAMP_HEADER]: String(timestamp),
      [SIGNATURE_HEADER]: await signAnswer(key, envelope.idempotency_key, timestamp, body),
    },
  });
}

/** What a delegate handler does with one item: answer in the vocabulary. */
export type Handler<Env = unknown, Ctx = unknown> = (envelope: Envelope, env: Env, request: Request, ctx: Ctx) => Promise<Answer> | Answer;

export type DelegateOptions<Env> = {
  /** The deployment key, or how to read it from the Worker's environment. */
  key: Key | ((env: Env) => Key | undefined);
  /** This side's clock, in Unix seconds; the system clock unless a test injects one. */
  now?: () => number;
};

/**
 * A Worker `fetch` handler for a call-mode delegate: verify the request, run `handler`,
 * sign the answer. A refused request answers 401 or 422 with no handler run. A handler that
 * throws answers 500, which the engine closes Transient and asks again on the next attempt.
 * An answer the handler produced outside the contract is a deterministic bug, so it goes
 * back as a signed `permanent` naming the fault, and the item ends until the binding's
 * `version` rotates.
 */
export function delegate<Env = unknown, Ctx = unknown>(
  handler: Handler<Env, Ctx>,
  options: DelegateOptions<Env>,
): (request: Request, env: Env, ctx: Ctx) => Promise<Response> {
  const now = options.now ?? systemNow;
  return async (request, env, ctx) => {
    const key = typeof options.key === "function" ? options.key(env) : options.key;
    if (key === undefined || key.length === 0) {
      return problem(500, "the deployment key is unset: bind the worker key the daemon signs under");
    }
    if (request.method !== "POST") {
      return problem(405, "a delegate takes `POST`");
    }
    let envelope: Envelope;
    try {
      envelope = await verifyRequest(request.clone(), key, now);
    } catch (error) {
      if (error instanceof DelegateRequestRejected) return problem(error.status, error.message);
      return problem(500, `the request could not be verified: ${describe(error)}`);
    }
    let answer: Answer;
    try {
      answer = await handler(envelope, env, request, ctx);
    } catch (error) {
      if (error instanceof DelegateAnswerInvalid) return fault(envelope, error, key, now);
      return problem(500, `the handler failed: ${describe(error)}`);
    }
    try {
      return await respond(envelope, answer, key, now);
    } catch (error) {
      if (error instanceof DelegateAnswerInvalid) return fault(envelope, error, key, now);
      return problem(500, `the answer could not be signed: ${describe(error)}`);
    }
  };
}

/** A signed `permanent` naming a deterministic handler bug, its reason bounded so it always leaves. */
async function fault(envelope: Envelope, error: DelegateAnswerInvalid, key: Key, now: () => number): Promise<Response> {
  const reason = `${error.name}: ${error.message}`.slice(0, REASON_CAP);
  try {
    return await respond(envelope, permanent(reason), key, now);
  } catch (inner) {
    return problem(500, `the fault could not be signed: ${describe(inner)}`);
  }
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function problem(status: number, message: string): Response {
  return new Response(JSON.stringify({ error: message }), { status, headers: { "content-type": "application/json" } });
}
