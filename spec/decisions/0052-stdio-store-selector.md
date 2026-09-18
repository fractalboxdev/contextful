# 0052 — A spawned engine finding no project manifest exits before writing protocol framing

**Status:** accepted 2026-09-18
**Decides:** `read.embed.refusal.store-selector`

## Context

On the process transport the client spawns the engine as a child and speaks newline-framed
JSON-RPC to it. There is no listener to address, no host to route to, and therefore no
store name anywhere in the wire protocol. Something else has to say which store the session
is against.

The child's working directory is that something: the process walks up from it looking for
the project manifest, and the first one it finds selects the store. This is the same rule a
command-line invocation follows, so one consumer and one shell in the same directory reach
the same data.

The walk can find nothing. A consumer spawned from a home directory, a temporary
directory, or a directory whose project was moved, has a working directory that names no
store. At that moment the process has not yet written a byte of protocol framing, and the
client on the other end is waiting for a handshake.

The transport keeps exactly one request in flight and resynchronizes by discarding exactly
one reply, so a half-open session that emits a handshake and then fails every call leaves
the client with framing it must interpret and a state it must unwind.

## Decision

The child's working directory selects the store by walking up for the project manifest.
Finding none, the process raises `StoreSelectorAbsent` and exits ahead of writing one byte
of protocol framing, so the client observes a failed spawn rather than a session that
answers nothing. The executable path and the working directory are trusted inputs,
executed and read as given.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse and exit before framing** *(chosen)* | A missing project is distinguishable from an empty store, and the client never holds a half-open session. | A consumer passing the wrong working directory gets a refusal rather than a usable default, and must know which directory it is spawning from. |
| Open a session against an empty in-memory store | Every consumer starts; no spawn-time failure to handle. | Loses on distinguishability: every read answers empty and the consumer reads a missing project as a quiet store. |
| Create a project at the working directory | Zero-config first run; the consumer always has somewhere to write. | Loses on surprise: a read verb lands state, and it lands it wherever the consumer happened to be spawned from. |
| Emit the handshake, then refuse each call | The client learns the reason on the call rather than from a spawn exit code. | Loses on session integrity: the transport's one-in-flight resynchronization assumes a live session, so a session that exists only to refuse is framing the client has to unwind. |
| Take the store from an explicit argument instead | No ambient state decides the target; the selection is written down. | Loses on consistency with the command line: one consumer and one shell in a directory would reach different stores, and the argument becomes required boilerplate for the common case. |

## Criteria

1. **Distinguishability of absence from emptiness** — whether a consumer can tell "there is
   no project here" from "the project here has no rows". *This criterion decided it.* The
   two call for opposite actions by the operator, and an engine that renders them
   identically makes the wrong one look correct. The empty-store option fails this
   completely and the project-creating option fails it by making the question moot in the
   worst direction.
2. **Session integrity** — whether a failed selection can leave the client holding partial
   framing. This eliminated refusing after the handshake.
3. **Surprise** — whether a read-shaped operation lands state. This eliminated project
   creation on its own, independently of the first criterion.
4. **Consistency with the command line** — whether one directory means one store across
   entry points. This ranked below the others; it decided the walk over an explicit
   argument, not the refusal itself.

## Consequences

A consumer that spawns from the wrong directory is refused rather than quietly served, and
diagnosing it means checking a working directory rather than checking why the store looks
empty.

The executable path and the working directory are trusted inputs. A caller that can set
either can choose what binary runs and what data it opens, so the process transport is
appropriate for a consumer running as its own user and inappropriate as a boundary between
two parties. That trust is the cost accepted, and it is not narrowable without giving up
the spawn.

Nothing about the refusal is recoverable in-band: there is no session to report it on, so
the reason travels as an exit status and a message on the error stream, which a client must
be written to read.

## Revisit triggers

- The process transport gains a store selector in the protocol itself, making the working
  directory one input rather than the only one.
- Consumers are observed wrapping the spawn in directory-probing logic, which says the
  refusal is being worked around rather than acted on.
- A deployment shape appears where the spawning party and the store owner are different
  parties, at which point trusting the executable path and directory is no longer sound.
