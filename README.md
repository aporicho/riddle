# MagicPaper (MP) — living magical paper for reMarkable Paper Pro Move

Write on the page with your pen. After a pause, MagicPaper **drinks your ink** —
your words fade into the paper — the page thinks for a moment, and an answer
writes itself back in a flowing hand, stroke by stroke, then fades away.

No screen glow, no keyboard, no chat UI. Just ink appearing on paper.

This fork is based on Maxime Rivest's original
[`riddle`](https://github.com/MaximeRivest/riddle) project and its
[demo](https://x.com/MaximeRivest).

## How this fork differs from upstream

MagicPaper 0.5.0 turns the original Tom Riddle diary into a Chinese-first,
Move-tested personal paper assistant while preserving the ink-only interface.

| Area | Upstream riddle | This MagicPaper fork |
|------|-----------------|----------------------|
| Device | reMarkable Paper Pro (`ferrari`) | Tested on Paper Pro Move (`chiappa`, OS 3.27), with standalone systemd launch/restore services |
| Identity | Tom Riddle's diary | MagicPaper (MP), the writer's concise magical servant |
| Entry and exit | AppLoad / five-finger exit | Three quick power presses enter or leave MP; one press still sleeps/wakes |
| AI path | pi or chat-completions | Responses API with vision, low reasoning, automatic background web search, and AI paper-ready editing |
| OCR path | Answer model reads the page image | Optional fast PP-OCRv6 first stage; the answer model then receives corrected text only |
| Handwriting recognition | Compact vision image | PP-OCRv6 plus contextual correction, optional PaddleOCR-VL fallback, or cropped high-detail vision with ambiguity and arithmetic checks |
| Reply appearance | Dancing Script | Three switchable Chinese handwriting fonts, per-glyph fallback, Traditional Chinese replies, and Chinese-aware wrapping |
| Memory | Short recent context plus saved pages | 20 recent dialogue turns, up to 400 saved pages, and a 40-page recall catalog |
| Automation | Conversation and page recall | Persistent recurring tasks, paper-native task/TODO/history lists, checkbox enable/disable, and due-time-aware smart heartbeat scheduling |
| Perceived latency | Request starts after the 2.8-second commit | OCR starts speculatively after one idle second; high-confidence complete input commits at 2.2s and uncertain input at 2.6s |
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
 pen ── raw strokes ──► MagicPaper ── idle 1s ──► speculative PNG request
                              │
                              └── idle 2.2/2.6s ─► 14-stage ink drinking
                              │                         │
                              │            ┌────────────┴────────────┐
                              │            ▼                         ▼
                              │       PP-OCRv6 text          direct vision image
                              │            └────────────┬────────────┘
                              │                         ▼
                              │                  Responses oracle
                              ▼                         │ answer text
 reply strokes ◄── selected font + 851 fallback ◄──────┘
   │
   ▼
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

One second without pen input begins reading a tentative page while the original
ink remains visible, including when PaddleOCR is enabled. Writing again cancels
the local wait and starts over with the latest page; the remote service may
still count the abandoned OCR job. High-confidence complete input and local
commands commit after 2.2 seconds; uncertain or incomplete input waits 2.6
seconds. The drink animation then retains all 14 stages at 50ms each.
Clean completed sentences begin writing as soon as they stream back. The blank
paper itself is the waiting state — there is no pulsing status dot.

## Tasks and TODOs

Recurring tasks and unscheduled TODOs are separate persistent lists. Write the
bare word `任务`, `任務`, or `task` to open the recurring-task page. Write the
bare word `TODO` in any capitalization to open the TODO page. On either page,
draw a horizontal line through an entry to delete it, or tap outside the rows
to return to the blank paper. Task rows also have a right-hand status box: a
check is active, a cross is paused, and tapping it toggles the state locally.

| Handwritten command | Effect |
|---------------------|--------|
| `任务 每五分钟讲一个黑暗冷笑话` | Add an active recurring task |
| `暂停任务 2` | Pause recurring task 2 |
| `恢复任务 2` | Resume task 2 after one fresh full interval |
| `修改任务 2 每十分钟提醒我喝水` | Replace task 2's interval and instruction |
| `删除任务 2` | Delete recurring task 2 without opening the list |
| `TODO 买牛奶` | Add “买牛奶” to the unscheduled TODO list |

Recurring tasks are limited to nine entries and have a minimum interval of
five minutes. Paused tasks remain visible but cannot become due. Resuming, or
modifying an active task, starts a fresh interval; missed runs are never
replayed. The smart heartbeat computes the nearest active due time and makes
no oracle/API request until then. Failed delivery retries after 30 seconds.
TODOs are limited to twenty visible entries and never participate in the
heartbeat.

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
export RIDDLE_OPENAI_MODEL="gpt-5.6-terra"              # vision only needed without separate OCR
export RIDDLE_OPENAI_API="responses"                    # or chat_completions
export RIDDLE_OPENAI_REASONING="low"                    # thinking models only
export RIDDLE_WEB_SEARCH="auto"                         # Responses mode
export RIDDLE_PAPER_REWRITE_MODEL="gpt-5.6-luna"        # rare format fallback
export RIDDLE_OPENAI_MAX_TOKENS="2000"                  # runaway guard
export RIDDLE_MEMORY_TURNS="20"                         # continuous dialogue
```

Without separate OCR, the answer model must be vision-capable. A standalone install reads
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

### Optional PaddleOCR handwriting stage

Set an AI Studio token to make `PP-OCRv6` read the committed page first.
MagicPaper submits the PNG as a multipart job, polls until it is ready, reads
the ordered `rec_texts` strings from the returned JSONL, and sends only that
text to the OpenAI-compatible answer model. The answer model still performs
contextual OCR correction, reasoning, background search, and paper-ready
writing. `PaddleOCR-VL-1.6` remains selectable for document-layout images.

```sh
export RIDDLE_OCR_TOKEN="your-aistudio-access-token"
export RIDDLE_OCR_URL="https://paddleocr.aistudio-app.com/api/v2/ocr/jobs"
export RIDDLE_OCR_MODEL="PP-OCRv6"
export RIDDLE_OCR_POLL_MS="250"
export RIDDLE_OCR_TIMEOUT_SECONDS="60"
```

Test OCR without spending an answer-model request:

```sh
riddle --ocr-test path/to/handwriting.png
```

Speculative OCR is on by default: MP submits after one second of idle time,
hiding 1.8 seconds of the commit delay. Set `RIDDLE_OCR_SPECULATIVE=off` if
avoiding possible paid orphan jobs after a mid-sentence pause matters more than
latency. Never commit a real OCR token to the repository.

Latency depends on OCR job time, the selected answer model, and whether search
is needed. The earlier direct-vision Terra configuration began a searched
quotation at about 7.2 seconds and completed at about 8.7 seconds. HTTPS is
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
MAGICPAPER_BUTTER_FONT=/path/to/ButterShiSan.ttf \
MAGICPAPER_851_FONT=/path/to/851LakeusNightWriting.ttf \
QUILL_DIR=../quill-move ./scripts/make-bundle.sh
```

The two `MAGICPAPER_*_FONT` variables are optional local TTF resources. They
are copied into `dist/riddle/fonts/` but never committed to this repository.
With both installed, handwrite **字体** or **字體**, tap a row to preview and
select it, then tap blank paper to leave. 851 is the default; the selection is
saved under `/home/root/riddle-data/preferences/font`. Missing glyphs fall
back to 851 automatically, so Simplified Task/TODO text does not disappear.
Do not publish a bundle containing fonts unless their licenses permit it.

Handwrite **历史** or **歷史** to open the nine newest local dialogue pages;
strike through a row to delete that memory and its saved strokes. In the task
list, the right-hand box is a direct local control: a check means active and a
cross means paused. User pen events are drained before commit, heartbeat, and
fade timers; touching an old lingering/fading reply clears it immediately and
starts the new stroke instead of making the writer wait.

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
