# Make masked `[env]` surrogates stable across starts

## Problem

A masked `[env]` entry reached the guest as a fresh random alphanumeric
string on every `airlock start`. Some tools persist the credential they
were started with: Codex writes `OPENAI_API_KEY` to its auth file on
first run and reads it back from there afterwards. With a new surrogate
per start, the persisted copy no longer matched what the proxy was
willing to swap, and the tool got a 401 on the second run.

The Codex preset makes this worse than a per-project problem: `~/.codex`
maps to `~/.airlock/codex/` on the host, which every project shares. So
the surrogate also had to agree *across projects*, not just across
restarts of one.

## Options considered

**Seed a PRNG with SHA-256 of the real value.** Rejected. The derivation
is public code, so the surrogate becomes an offline guessing oracle:
compute the surrogate for a candidate secret, compare. Security drops
from "leaks only the length" to "as strong as the secret's entropy". Fine
for a 40-character API key, fatal for a masked database URL or basic-auth
password. It is the unsalted password hash problem.

**HMAC with a host-side random key.** Secure and stable, but the key
would have to be host-global (per-project keys break the shared
`~/.codex` case), which adds a key file to create, protect and explain.
And the unpredictability it buys is not needed: see below.

**Derive from the variable name and the value's byte length.**
Chosen. The surrogate then carries zero bits about the value beyond its
length, which the same-length contract already reveals. Predictability
is harmless because the surrogate is handed to the untrusted guest by
design, and the proxy that swaps it is reachable only from that guest
over vsock. There is no host listener a local process could hit with a
guessed surrogate, and injection is scoped to the rule's `allow` targets,
so a planted surrogate in a README or a response body cannot make the
proxy inject anywhere it would not already.

Side effects, all wanted: the same name and length give the same
surrogate in every project on every machine, so the shared Codex auth
file works everywhere. Rotating the real secret keeps the surrogate as
long as the length is unchanged, so cached credentials survive rotation.

## Implementation

`surrogate_for(name, value)` in `project/sandbox_env.rs` hashes a domain
separator (`airlock-surrogate-v1`), the length-prefixed name and the
byte length with SHA-256, seeds a `ChaCha20Rng` with the digest, and
draws one `u32` per byte, modulo the 62-symbol alphabet. ChaCha20's
stream is stable across `rand` versions, unlike `StdRng`, so the
surrogate does not change on a dependency bump. The `chacha` feature of
`rand` provides the generator; the `chacha20` crate was already in the
lockfile through `chacha20poly1305`.

The surrogate is ASCII, so it has the same byte length as the real
value. Byte length rather than character count (which the old random
surrogate matched) because the proxy rewrites headers as bytes: a
surrogate of the same size keeps needle and replacement equal, which an
in-place rewrite could rely on later. The minimum inject length is now a
byte count too.

Bump the domain separator if the derivation ever has to change, and
document that cached credentials need a re-login.

## Tests

`surrogate_depends_only_on_name_and_length` pins that the value does not
influence the result and that name or length changes do.
`surrogate_name_and_length_do_not_alias` checks the length prefix
(`TOKEN1` + short vs `TOKEN` + long). `surrogate_is_pinned` holds golden
values for three inputs, including a 100-character one that spans more
than one ChaCha block.

## Docs

`configuration/env.md` no longer says "regenerated on every start" and
states that the surrogate is derived from the name and the length. The
Codex, Claude Code and Copilot preset pages drop the word "random"; the
Codex page notes that the persisted copy stays valid.
