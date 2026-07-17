# MagicPaper (MP) — living magical paper for reMarkable Paper Pro Move

Write on the page with your pen. After a pause, MagicPaper **drinks your ink** —
your words fade into the paper — the page thinks for a moment, and an answer
writes itself back in a flowing hand, stroke by stroke, then fades away.

No screen glow, no keyboard, no chat UI. Just ink appearing on paper.

This fork is based on Maxime Rivest's original
[`riddle`](https://github.com/MaximeRivest/riddle) project and its
[demo](https://x.com/MaximeRivest).

## How this fork differs from upstream

MagicPaper 0.4.2 turns the original Tom Riddle diary into a Chinese-first,
Move-tested personal paper assistant while preserving the ink-only interface.

| Area | Upstream riddle | This MagicPaper fork |
|------|-----------------|----------------------|
| Device | reMarkable Paper Pro (`ferrari`) | Tested on Paper Pro Move (`chiappa`, OS 3.27), with standalone systemd launch/restore services |
| Identity | Tom Riddle's diary | MagicPaper (MP), the writer's concise magical servant |
| Entry and exit | AppLoad / five-finger exit | Three quick power presses enter or leave MP; one press still sleeps/wakes |
| AI path | pi or chat-completions | Responses API with vision, low reasoning, automatic background web search, and AI paper-ready editing |
| Handwriting recognition | Compact vision image | Cropped high-detail input up to 1600 px, ambiguity checks, arithmetic consistency, and digit-by-digit review |
| Reply appearance | Dancing Script | ChenYuluoyan Chinese handwriting, Traditional Chinese replies, and Chinese-aware wrapping |
| Memory | Short recent context plus saved pages | 20 recent dialogue turns, up to 400 saved pages, and a 40-page recall catalog |
| Automation | Conversation and page recall | Persistent recurring tasks such as `任务 每五分钟……`, checked by a five-minute heartbeat |
| Perceived latency | Request starts after the 2.8-second commit | Speculative request after one idle second, cancel/restart if writing resumes, then stream the first clean sentence |
| E-ink behavior | Thinking indicator and broader refreshes | No pulsing wait dot; reply-region cleanup avoids a distracting full-screen refresh after every answer |

The upstream commit history and MIT attribution are intentionally retained.

### 🪄 New to this? Start here

You need a **reMarkable Paper Pro Move** in developer mode with a launcher installed.
If that sounds like a lot, it isn't — **[remagic](https://github.com/maximerivest/remagic)**
walks you through turning on developer mode and sets up everything with one
command. Come back here, drop MagicPaper in, and start writing.

Already have xovi + AppLoad? The remagic catalog installs the upstream app;
to get the MagicPaper changes in this fork, [build this source](#building)
and stage its bundle as described below.

### Install upstream with remagic

```sh
remagic install riddle     # checksum-verified download → AppLoad
remagic config riddle      # settings form in your browser (+ QR for phone)
```

Then in **AppLoad**: tap **Reload**, then **MagicPaper**. Write, and rest your
pen. (Or install it from the **Store** app right on the tablet.)

### Install this fork's bundle

This fork does not currently publish a prebuilt GitHub release. Build it with
the takeover instructions below; `scripts/make-bundle.sh` produces the
self-contained `dist/riddle` directory.

1. Build and stage `dist/riddle` using [Building](#building).
2. Copy the folder to your tablet:
   `scp -O -r dist/riddle root@10.11.99.1:/home/root/xovi/exthome/appload/`
3. Add an API key: `cp oracle.env.example oracle.env` in that folder and put your `RIDDLE_OPENAI_KEY` in it (any OpenAI-compatible key). Or skip it to use [pi](#option-b--pi-the-power-path).
4. In **AppLoad**: tap **Reload**, then **MagicPaper**. Write, and rest your pen.

> ⚠️ **This modifies your device.** The prebuilt bundle and the catalog build
> run in **takeover mode**: opening MagicPaper stops the whole reMarkable UI
> and takes the screen. Leave with a **5-finger tap** — xochitl restarts
> automatically. It runs as root and drives the e-ink engine directly. This
> fork has been tested on a **reMarkable Paper Pro Move** (chiappa,
> aarch64, OS 3.27). It may not work on other models or OS versions, and you use
> it entirely at your own risk. Not affiliated with reMarkable AS. Keep SSH
> access working before you install anything — if anything ever wedges:
> `ssh root@10.11.99.1 'systemctl start xochitl'`.

## How it works

```
 pen (raw evdev, full 4096-level pressure, hardware event rate)
   │ strokes
   ▼
 MagicPaper ── idle 1s → speculative PNG request ──► Responses oracle
   │            └─ writing resumes: cancel and restart
   │          idle 2.8s → commit and dissolve
   ▼ strokes (ChenYuluoyan → skeletonized to single-pixel pen paths)
 display backend
   ├── qtfb        — windowed, inside xochitl (build-from-source flavour)
   └── quill       — full takeover: xochitl stopped, vendor e-ink engine
                     driven directly for instant ink (lowest latency there
                     is; what the prebuilt bundle runs)
```

- **This repository** — the app (Rust). Pen input, ink surface, handwriting
  synthesis (rasterize → Zhang-Suen thinning → stroke tracing → animated
  replay), the oracle process manager, and both display backends.
- **[Quill](https://github.com/MaximeRivest/quill)** — the sibling takeover display host (C/C++). A
  clean-room, MIT-licensed adapter over the vendor `libqsgepaper.so` waveform
  engine, exposed as a small C ABI (`quill_init` / `quill_buffer` / `quill_swap`)
  that riddle links against with `--features takeover`. Also carries a small
  family of demos (`scribble`, a pen-to-glass latency test, plus map, image,
  and GIF renderers).

## Gestures

| Do this | And |
|---------|-----|
| Write, then rest the pen | MagicPaper drinks your ink and replies |
| Write *"show me what I wrote about…"* | The remembered page **rises through the paper**: the date, your own handwriting rewriting itself stroke by stroke, and MP's old reply — all in faded ink. Touch the pen anywhere and today's page returns |
| Write *"what do you remember?"* | MP answers with a handwritten list of remembered moments |
| Flip the marker | Erase |
| Draw a large **?** | Summon the built-in guide |
| Tap five fingers at once | Leave the diary *(takeover mode)* |
| Power button once | The page turns to *"The diary sleeps."*, then the tablet suspends; press again to wake exactly where you were *(takeover mode)* |
| Power button three times quickly | Open the standalone diary from xochitl, or leave it while the diary is open |

In the windowed (qtfb) flavour, xochitl keeps the touchscreen and the power
button: close the diary from AppLoad instead.

After one second without pen input, Responses mode begins reading a tentative
page in the background while the original ink remains visible. The page is
only committed and dissolved after the existing 2.8-second pause; writing or
erasing before then discards that tentative answer and restarts the process.
Clean completed sentences begin writing as soon as they stream back. The blank
paper itself is the waiting state — there is no pulsing status dot.

## MagicPaper remembers

Every finished page is kept — your actual pen strokes, a transcription, and
MP's reply — so MagicPaper can do three things:

- **Follow the conversation.** Recent pages ride along with each request, so
  MP remembers what you wrote yesterday (both backends, same behavior).
- **Conjure the past.** Ask in ink — *"show me the page about the garden"*,
  *"find what I wrote on Tuesday"* — and the diary rewrites that page in
  front of you, in your own hand, dated, in faded ink. No buttons, no lists,
  no chrome: the pen is the only interface.
- **Answer from memory.** *"What do you remember?"* gets a handwritten index.

Memories live only on the tablet, in plain files under
`/home/root/riddle-data/memories` (delete the folder and the diary forgets;
the last ~400 pages are kept). `RIDDLE_MEMORY=off` in `oracle.env` turns all
of it off — no storage, and nothing extra sent with requests. Set
`RIDDLE_TZ_OFFSET` (hours from UTC) so memory dates read right.

## The oracle (the "spirit" in the diary)

The diary's replies come from a vision LLM that reads your handwriting from the
committed page (sent as an inline PNG). There are **two backends**, chosen at
startup — pick whichever you have:

### Option A — any OpenAI-compatible API (easiest, zero setup)

Set an API key and riddle talks straight to an OpenAI-compatible HTTP API.
Legacy chat-completions works with OpenRouter, Groq and local servers;
Responses mode adds model-managed background web search and paper-ready
answer editing. No extra software runs on the tablet.

```sh
export RIDDLE_OPENAI_KEY="sk-..."                       # required
export RIDDLE_OPENAI_BASE="https://api.openai.com/v1"   # optional (default)
export RIDDLE_OPENAI_MODEL="gpt-5.6-terra"              # must see images
export RIDDLE_OPENAI_API="responses"                    # or chat_completions
export RIDDLE_OPENAI_REASONING="low"                    # thinking models only
export RIDDLE_WEB_SEARCH="auto"                         # Responses mode
export RIDDLE_PAPER_REWRITE_MODEL="gpt-5.6-luna"        # rare format fallback
export RIDDLE_OPENAI_MAX_TOKENS="2000"                  # runaway guard
export RIDDLE_MEMORY_TURNS="20"                         # continuous dialogue
```

Any vision-capable model works. A standalone install reads
`/home/root/.config/riddle/oracle.env`; legacy AppLoad bundles also accept an
`oracle.env` next to the binary. See `oracle.env.example`. Example with
OpenRouter:

```sh
export RIDDLE_OPENAI_KEY="$OPENROUTER_API_KEY"
export RIDDLE_OPENAI_BASE="https://openrouter.ai/api/v1"
export RIDDLE_OPENAI_MODEL="openai/gpt-4o-mini"
```

Two gotchas with reasoning models: set `RIDDLE_OPENAI_REASONING=low` for
faster first ink (some providers reject the field on non-reasoning models —
leave it unset there), and keep
`RIDDLE_OPENAI_MAX_TOKENS` roomy — hidden reasoning tokens count against it,
and a tight cap starves the visible reply.

Verify your setup before launching the diary:

```sh
riddle --oracle-test path/to/handwriting.png   # prints the streamed reply
```

Latency depends on the selected model and whether search is needed. With the
tested Terra configuration, a searched quotation began streaming to paper at
about 7.2 seconds and completed at about 8.7 seconds; the one-second
speculative start hides 1.8 seconds of the original commit wait. HTTPS is
built into riddle (pure Rust, no extra libraries).

### Option B — pi (the power path)

If you already run [`pi`](https://github.com/badlogic/pi-mono), riddle will use
a resident `pi --mode rpc` process kept warm (Node + your subscription auth
loaded once), so each turn pays only model latency. Used automatically when
`RIDDLE_OPENAI_KEY` is **not** set. Defaults (override in `oracle.env`):
pi at `/home/root/node/bin` (`RIDDLE_PI_BIN_DIR`), provider `openai-codex`
(`RIDDLE_PI_PROVIDER`), model `gpt-5.4-mini` (`RIDDLE_PI_MODEL`).

Both stream the reply sentence-by-sentence, so the quill starts writing seconds
before the model finishes. The persona prompt lives in `src/oracle.rs`.

With the HTTP backend, the most recent 20 page/reply pairs are sent as
continuous dialogue on every turn. Both backends also receive the fresh
40-page recall catalog; the full local archive retains up to 400 pages.

If the oracle can't answer — missing key, refused key, no Wi-Fi — MP writes
the reason on the page instead of a reply, and the full error goes to the
journal (`journalctl -u riddle-takeover`).

## Building

Cross-compiled from x86_64. Two flavours:

### Windowed (AppLoad/qtfb) — build from source

The bundles above are the takeover flavour; the windowed flavour must be
built. Requires [xovi + AppLoad](https://github.com/asivery/rm-appload) on
the device.

```sh
git clone https://github.com/aporicho/riddle
cd riddle
cargo build --release --target aarch64-unknown-linux-gnu
```

Install the binary to `/home/root/xovi/exthome/appload/riddle/` with an
`external.manifest.json` that sets `"qtfb": true` and points `"application"`
at the binary itself (the manifest in this repo is the takeover one — AppLoad
only hands riddle a window, via `QTFB_KEY`, when `qtfb` is true).

### Takeover on Paper Pro Move

Requires the chiappa reMarkable SDK toolchain (tested with 3.27) because the
linked vendor Qt libs need its glibc, **and** `libqsgepaper.so` pulled from
*your own device* (it is proprietary and not distributed here):

```sh
# Keep the quill-move and riddle repositories beside each other.
cd quill-move
RM_SDK=~/rm-sdk-chiappa-3.27 ./build.sh
cd ../riddle
RM_SDK=~/rm-sdk-chiappa-3.27 QUILL_DIR=../quill-move ./build-takeover.sh
QUILL_DIR=../quill-move ./scripts/make-bundle.sh
```

The staged `dist/riddle/` is self-contained (binary, `libquill.so`, launch
scripts, manifest) — copy it to
`/home/root/xovi/exthome/appload/riddle/`, or publish it to the catalog with
`remagic publish dist/riddle`. Launching via AppLoad (`appload-launch.sh`)
detaches into a transient systemd unit, stops xochitl, runs the diary, and
**always restores xochitl on exit** — leave with a 5-finger tap or SIGTERM
(`systemctl stop riddle-takeover`); one power press sleeps and wakes the
diary without leaving it, while three quick presses enter/exit. The unit's stop hook restarts xochitl even if
riddle dies uncleanly. If anything wedges:
`ssh root@10.11.99.1 'systemctl start xochitl'`.

## What leaves the device

- Each committed page is rasterized to a small grayscale PNG and sent to the
  oracle **you** configured — nothing else ever leaves the tablet, and there
  is no telemetry.
- The PNG (`/tmp/riddle-page.png`) is deleted as soon as the oracle has read
  it; set `RIDDLE_KEEP_PAGE=1` to keep the last page around for debugging.
- riddle never writes replies to disk. The pi backend, however, keeps its own
  session history in its data dir — the HTTP backend keeps nothing.
- MP stays in character by design: the persona prompt (see `src/oracle.rs`)
  tells the model it is living magical paper and nothing else.

## Fonts

The reply hand is [ChenYuluoyan 2.0 Thin](https://github.com/Chenyu-otf/chenyuluoyan_thin),
with character-aware line wrapping for unspaced Chinese text (SIL OFL 1.1 —
see `fonts/OFL-ChenYuluoyan.txt`).

## License

MIT for everything in this repository (see `LICENSE`). The vendor libraries it
interposes (`libqsgepaper.so`, Qt) are **not** included and must come from
your own device/SDK.
