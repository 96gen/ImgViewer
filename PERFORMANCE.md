# ImgViewer performance and RAM verification

This report records the local before/after measurement for the 0.1.1 memory work and the 0.1.2 no-flicker image-swap verification. It is evidence for this checkout, not a universal hardware requirement.

## Environment

- Date: 2026-07-22
- OS: Microsoft Windows NT 10.0.26200.0 x64
- Logical processors visible to the process: 16
- WebView2 runtime: 150.0.4078.83
- Measurement: `scripts/smoke-memory.ps1`; Windows UI Automation, Win32 process counters, no WDIO/WebDriver
- Scope: the root `ImgViewer.exe` plus its recursive `msedgewebview2.exe` descendants

## Large-image before/after

Both builds used the same machine and script parameters:

```powershell
-Cycles 6 -Warmup 1 -Width 6000 -Height 6000 -IdleMilliseconds 200
```

The three generated PNG files contain 36,000,000 pixels each. Short-run retained-growth numbers are intentionally omitted because WebView2 has not reached its cache/GC pressure plateau within six cycles.

| Metric | 0.1.0 before | 0.1.1 after | Observed change |
| --- | ---: | ---: | ---: |
| Root process peak private bytes | 144.88 MiB | 5.95 MiB | -138.93 MiB (-95.9%) |
| Root process peak working set | 131.82 MiB | 29.34 MiB | -102.48 MiB (-77.7%) |
| Full process-tree peak private bytes | 378.04 MiB | 375.23 MiB | -2.81 MiB (-0.7%) |
| Full process-tree peak working set | 573.16 MiB | 561.79 MiB | -11.37 MiB (-2.0%) |
| p95 UIA-visible load time | 317 ms | 236 ms | -81 ms (-25.6%) |

The root-process reduction is the direct result of removing the redundant full-frame decode for pass-through JPG/PNG/GIF/WebP. Full-tree reduction is smaller because WebView2 must still allocate the final decoded pixels it displays. The load-time result is an observed local comparison, not a cross-machine performance guarantee.

## Long-run retention gate

The final 0.1.1 default run used 100 cycles with the first 70 treated as WebView2 warm-up. WebView2 private commit climbed until normal memory pressure triggered reclamation around cycle 70, then the evaluated tail was stable:

- retained private change: -14.77 MiB
- private slope: -0.570 MiB/cycle
- retained working-set change: -3.80 MiB
- working-set slope: -0.208 MiB/cycle
- root peak private bytes: 6.36 MiB
- root peak working set: 31.08 MiB
- p95 UIA-visible load time: 324 ms

PASS anchor:

```text
PASS memory-smoke cycles=100 warmup=70 images=3 size=2048x1536 retained-private-mib=-14.77 private-slope-mib-per-cycle=-0.570 retained-working-set-mib=-3.80 working-set-slope-mib-per-cycle=-0.208 peak-root-private-mib=6.36 peak-root-working-set-mib=31.08 p95-load-ms=324 webdriver=absent
```

## 0.1.2 no-flicker swap gate

Version 0.1.2 keeps the committed image visible while one candidate Blob is read and predecoded. It atomically publishes the candidate only when it is still the newest generation, waits for the next paint, then revokes the old Blob. Stale candidates are canceled and revoked. This deliberately allows a short-lived overlap of one visible image and one candidate, but no multi-image prefetch or long crossfade.

The standard 100-cycle run passed all configured gates. Its evaluated 30-cycle tail showed stable private commit but a temporary rise in resident working set:

- retained private change: -6.98 MiB
- private slope: -0.286 MiB/cycle
- retained working-set change: +26.76 MiB
- working-set slope: +1.157 MiB/cycle
- full process-tree peak private / working set: 523.24 / 489.06 MiB
- root peak private / working set: 6.29 / 29.70 MiB
- p95 UIA-visible load time: 260 ms

Because the resident-set tail was still rising, a second run was extended to 160 cycles with 110 warm-up cycles and a 50-cycle evaluated tail. That longer tail plateaued:

- retained private change: -0.17 MiB
- private slope: +0.015 MiB/cycle
- retained working-set change: -3.89 MiB
- working-set slope: -0.044 MiB/cycle
- full process-tree peak private / working set: 513.43 / 491.47 MiB
- root peak private / working set: 6.45 / 29.91 MiB
- p95 UIA-visible load time: 276 ms

Extended-run PASS anchor:

```text
PASS memory-smoke cycles=160 warmup=110 images=3 size=2048x1536 retained-private-mib=-0.17 private-slope-mib-per-cycle=0.015 retained-working-set-mib=-3.89 working-set-slope-mib-per-cycle=-0.044 peak-root-private-mib=6.45 peak-root-working-set-mib=29.91 p95-load-ms=276 webdriver=absent
```

The final packaged continuity smoke also switched a solid red PNG to a 24,000,000-pixel green TIFF through the production single-instance handoff. Five 10 ms UI Automation samples all contained an Image element and the native window rectangle remained unchanged. Framebuffer color sampling was unavailable in this desktop session and is explicitly not claimed:

```text
PASS switch-continuity uia-image-min=1 pixel=skipped samples=5 trigger=single-instance-handoff rect=unchanged webdriver=absent
```

## Interpretation limits

- Private commit and working set are different. WebView2 may retain committed address space while physical working set has already plateaued.
- The 100-cycle 0.1.2 working-set increase is retained above rather than discarded; the 160-cycle run shows that this local WebView2 session later plateaued, not that every machine will plateau at the same cycle.
- GPU driver, DPI, WebView2 version, window size and animation content affect absolute values. Compare before/after only with identical parameters on the same machine.
- UI Automation proves that the target image element became visible. It does not prove a framebuffer pixel or animated-frame transition; those remain part of the separate native pixel smoke.
