# Patched cpal 0.15.3

This is a vendored copy of [cpal 0.15.3](https://crates.io/crates/cpal/0.15.3)
with one local patch. The workspace `Cargo.toml` redirects the `cpal` crate
dependency to this directory via `[patch.crates-io]`.

## Why

Stock cpal 0.15.3 busy-spins one full CPU core whenever `stream.pause()` is
called against an ALSA device backed by PipeWire-Pulse (i.e. PipeWire's
PulseAudio compatibility server). The transcriber hits this path on every
mic mute, taking the process to ~100% CPU while muted.

Upstream tracking: [RustAudio/cpal#785 — *[Request] Sleep thread on pause*](https://github.com/RustAudio/cpal/issues/785)
(open). The reporter describes the same bug shape and the same fix shape;
no upstream resolution as of cpal 0.15.3.

Mechanism: cpal's ALSA worker thread loops over `alsa::poll()`. When the PCM
is paused via `snd_pcm_pause()`, the ALSA pulse plugin keeps signalling poll
descriptors as ready (with revents that don't translate to `IN` or `OUT`),
so the worker hits the *"Nothing to process, poll again"* branch
(`mod.rs:749` in upstream) and continues the loop with no sleep.

## What changed

All edits are in `src/host/alsa/mod.rs`, grep for `cpal#785`:

1. `use std::sync::{Arc, Condvar, Mutex};` — added `Condvar`.
2. `StreamInner` gains a `pause_signal: (Mutex<bool>, Condvar)` field. Default
   is `(false, …)`.
3. New helper `wait_while_paused(stream)` blocks on the condvar while the
   flag is set.
4. `input_stream_worker` and `output_stream_worker` call `wait_while_paused`
   at the top of each loop iteration, before `poll_descriptors_and_prepare_buffer`.
5. `StreamTrait::pause` sets the flag to `true` before calling
   `channel.pause(true)`.
6. `StreamTrait::play` clears the flag and `notify_all`s the condvar before
   calling `channel.pause(false)`.
7. `Drop for Stream` clears the flag and `notify_all`s before signalling the
   self-pipe — otherwise a worker blocked in the condvar would never observe
   the shutdown signal and `join` would deadlock.

Total change: ~30 lines. Touches only the ALSA backend; other backends
(CoreAudio, WASAPI, …) are untouched.

## Verifying the patch

While the transcriber runs, mute the mic and sample the `cpal_alsa_in`
thread's CPU. With this patch it sits at 0%; with stock cpal it sits at
~100%+.

```bash
pid=$(pgrep -x transcriber)
for t in /proc/$pid/task/*/stat; do
  awk -v t="$t" '{ if ($2 == "(cpal_alsa_in)") print t, $14+$15 }' "$t"
done
```

Take two readings a second apart and compare the tick counts.

## Updating cpal

When upstream cpal merges a real fix (track cpal#785 / cpal#284), drop this
vendor directory and remove the `[patch.crates-io]` block from
`transcriber-rs/Cargo.toml`.

If you only need to bump to a newer cpal that still has the spin:

1. `rm -rf transcriber-rs/vendor/cpal`
2. `cp -r ~/.cargo/registry/src/index.crates.io-*/cpal-NEW_VERSION transcriber-rs/vendor/cpal`
3. Re-apply the seven edits above (each is small; the grep tag `cpal#785`
   marks the exact insertion points to look for).
4. `cargo build --release` to confirm.
5. Verify 0% paused CPU on the `cpal_alsa_in` thread (see above).
