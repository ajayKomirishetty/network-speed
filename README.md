# Voyis Network Speed

A small Windows desktop tool that lets someone who isn't comfortable with a
terminal run an iperf3 network throughput test, watch it live, and save the
result.

Built with **Rust** and **egui** (via `eframe`) for the Voyis Imaging Inc.
take-home assignment.

---

## How to build and run

### Prerequisites

- Rust toolchain (stable): https://rustup.rs
- `iperf3` available at runtime (the Windows installer bundles it; see below)

### Run the app (any desktop OS, for development)

```sh
cargo run --release
```

### Build a Windows binary on Linux (cross-compile)

```sh
# one-time setup
sudo apt install mingw-w64
rustup target add x86_64-pc-windows-gnu

cargo build --release --target x86_64-pc-windows-gnu
# -> target/x86_64-pc-windows-gnu/release/voyis-network-speed.exe
```

The linker setting lives in `.cargo/config.toml`.

### Run the tests

```sh
cargo test
```

The integration tests in `src/iperf/runner.rs` exercise a **real** iperf3
binary (client against a local `iperf3 -s` server): live interval events,
summary parsing, cancellation, and the connection-refused error path. They
are skipped with a notice if iperf3 is not installed.

### Build the Windows installer

```sh
sudo apt install nsis   # provides makensis
cd installer
makensis voyis-network-speed.nsi
# -> installer/VoyisNetworkSpeed-Setup.exe
```

The installer (NSIS-generated `.exe`):

- installs to `C:\Program Files\Voyis Network Speed`
- adds a **Start Menu** shortcut (plus an Uninstall shortcut)
- registers in **Add/Remove Programs**
- uninstalls cleanly: removes the app, the bundled iperf3, the shortcuts,
  and the registry entry

## Framework choice and architecture

**Language: Rust.** A single static binary with no runtime to install, which
keeps the installer small and the deployment story trivial. Rust's ownership
model also makes the threaded design below safe by construction.

**GUI: egui via eframe** (immediate-mode GUI), with `egui_plot` for the live
chart. egui was chosen over retained-mode frameworks (WPF/WinForms/Qt)
because:

- the UI is re-rendered from plain structs every frame, so live data (the
  current Mbps number, the chart, the progress bar) updates with no
  data-binding machinery;
- the whole UI is one cross-platform codebase — it can be developed and
  smoke-tested on Linux/macOS and shipped as a native Windows binary;
- no XAML/designer tooling or .NET runtime dependency.

**Architecture** (`src/`):

- `main.rs` — the egui application: configuration form, live readouts,
  chart, summary cards, export buttons, and the iperf3 path picker. It owns
  no I/O; it only drains an `mpsc` channel once per frame and repaints while
  a test runs.
- `iperf/runner.rs` — spawns `iperf3` as a child process on a worker thread,
  streams stdout/stderr through a channel as `TestEvent`s, and implements
  cancellation (`Child::kill` + `wait`, then joins the reader threads).
- `iperf/parser.rs` — parses iperf3's human-readable output lines into
  `ThroughputSample`s and the final `TestSummary`.
- `iperf/mod.rs` — `resolve_iperf3()`: startup detection of the iperf3
  binary (custom path → bundled copy → `PATH`).
- `models.rs` — `ThroughputSample`, `TestSummary`, `TestEvent`.
- `settings.rs` — persists the user's custom iperf3 path as JSON in the
  platform config dir.
- `export.rs` — CSV / JSON / TXT result export.

**Threading model (why the UI never freezes):** the iperf3 child process and
both pipe readers live on a dedicated worker thread. The UI thread only calls
`try_recv()` in a loop each frame — it never blocks on the process. While a
test runs, the app calls `request_repaint_after(100ms)` so live values update
without busy-looping.

## How iperf3 output is parsed, and why

The app runs iperf3 in its **default human-readable text mode** (TCP is
iperf3's default; no `-u` flag is passed):

```text
iperf3 -c <host> -p <port> -t <duration> -i 1 --forceflush
```

and parses stdout line-by-line — **not** `-J/--json`. Reason: JSON mode only
emits the complete document when the process exits, which makes per-interval
live updates impossible. Text mode with `-i 1 --forceflush` streams one
interval line per second through the pipe as the test runs, which is exactly
what the "live results" requirement needs.

- **Interval lines** (`[  5] 0.00-1.00 sec 1.10 MBytes 9.24 Mbits/sec ...`)
  are tokenized with `split_whitespace` and matched positionally:
  interval range → transfer value+unit → bitrate value+unit → optional
  retransmit count. Units are converted explicitly (byte units use 1024-based
  multipliers, bit units 1000-based, matching iperf3's own conventions).
- **Summary lines** (the trailing lines ending in `sender` / `receiver`) give
  the final sender and receiver throughput, total bytes sent/received, and —
  on the sender line — the total retransmit count.
- Header lines (`[ ID] Interval ...`), the `iperf Done.` trailer, and anything
  that doesn't match the expected shape are ignored rather than erroring, so
  minor iperf3 formatting differences degrade gracefully.

## iperf3 dependency: bundled, with an override

**Decision: the installer bundles iperf3** (`iperf3.exe` + `cygwin1.dll`,
iperf **3.21** 64-bit Windows build) alongside the app, and its BSD-3-Clause
license is included as `LICENSE.iperf3.txt` in the install directory.

Justification: the target user "isn't comfortable with a terminal", so the
tool must work immediately after install with zero extra steps. Requiring a
separate iperf3 install would reintroduce exactly the friction this tool
exists to remove.

The app still **detects iperf3 at startup** and shows the detected version
(or a red "not detected" state that disables Start). The user can always set
a **custom path** — via the text field or the Browse button — which is
persisted across launches. Resolution order: custom path → bundled copy next
to the exe → `PATH`. This keeps IT-managed environments happy (they can point
the app at their own vetted iperf3) without punishing everyone else.

## Error handling

| Situation | Behavior |
|---|---|
| iperf3 not found at startup (or custom path invalid) | Red "iperf3 not detected" banner; Start button disabled until a valid binary is chosen via Browse |
| iperf3 binary missing when a test starts | Clear message naming the configured path; no panic, no hang |
| Server unreachable / connection refused | Friendly message ("Could not connect to host:port… make sure an iperf3 server is running…") with the raw iperf3 details attached |
| Unresolvable hostname | "Could not resolve the server name…" hint |
| No route to host / network unreachable | Network-path hint (VPN, correct network) |
| Server closes the connection mid-test | Explains the server dropped the connection, with details |
| Non-zero iperf3 exit, other stderr | Surfaced verbatim with the exit status |
| Invalid host / port / duration input | Inline validation errors before anything is spawned |
| Cancel pressed | `Child::kill()` + `wait()`, reader threads joined, `Cancelled` state; verified no iperf3 process remains |
| Export write failure | Message naming the file and the OS error |

Retransmits are shown as `n/a` where the platform/output doesn't report them
(the receiver summary line carries no retransmit count).

## Known limitations

- **TCP only**, per the assignment (iperf3 defaults to TCP; the app never
  passes `-u`).
- Live chart resolution is **one sample per second** (`-i 1`); sub-second
  granularity isn't available from iperf3's text interval reporting.
- Retransmit counts come from the **sender** side; iperf3 doesn't report them
  for the receiver direction.
- Byte/bit unit parsing covers `Bytes`→`TBytes` and `bits/sec`→`Tbits/sec`;
  anything else on a line causes that line to be skipped, not the test to fail.
- The Windows installer is built with NSIS on Linux; it is **not code
  signed** (not required). Windows SmartScreen may show a warning on first
  run.
- Settings are stored per-user; a custom iperf3 path set by one Windows user
  doesn't apply to others.
- `iperf3 --version` is executed once at startup for detection — a malicious
  binary placed at the configured path would be executed with the user's own
  privileges, same as any tool in `PATH`. Prefer the bundled copy or a
  trusted location.

## Versions

- **iperf3 3.21** — bundled with the Windows installer (build from
  https://github.com/ar51an/iperf3-win-builds, linked from iperf.fr);
  license: BSD-3-Clause (`installer/third-party/LICENSE.iperf3.txt`).
- Linux development/testing used the distribution iperf3
  (`iperf3 --version` on the build machine).
- Any iperf3 **≥ 3.5** works: the parser only depends on the long-stable
  text interval/summary format.

## Project layout

```text
src/
  main.rs            egui application (UI + state machine)
  models.rs          ThroughputSample / TestSummary / TestEvent
  iperf/
    mod.rs           resolve_iperf3() startup detection
    runner.rs        process management, cancellation, error mapping
    parser.rs        text output parsing (+ unit tests)
  settings.rs        persisted custom iperf3 path
  export.rs          CSV / JSON / TXT export (+ unit tests)
installer/
  voyis-network-speed.nsi   NSIS installer script
  third-party/              bundled iperf3.exe, cygwin1.dll, LICENSE.iperf3.txt
```
