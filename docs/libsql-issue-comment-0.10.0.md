**Data point: libsql 0.10.0 (final) no longer reproduces this on my machine.**

Same harness as my previous comment (a parent re-execs the child and reads its exit code), same Windows 11 box, rustc 1.99.1, release build, `libsql = "0.10.0"` — the only change from the 0.9.30 / 0.10.0-pre.4 runs is the dependency version.

| arm | 0.9.30 | 0.10.0 final |
|---|---|---|
| sequential, current-thread runtime, distinct file per `connect()` (the no-concurrency reproducer above) | 6/8 runs, first fault at 2,272 connects | **0/10**, 27,920–55,888 connects per run, ~378,000 in total |
| 48 concurrent opens per round, first query on each, 15 s per run | 6/6 | **0/6**, ~38,000 connects per run, ~229,000 in total |
| open storm beside a saturated long-lived database (my harness's `control` arm) | faulted (2/12 at 32 opens, 5/12 at 128) | **0/6** |

At the ~1 in 20,000–25,000 per-`connect()` rate I measured on 0.9.30, the sequential arm alone would predict 15–19 faults; it had none.

Outside the reproducer, the same picture: in my project's test suite, a benchmark that faulted 6/10 on 0.9.30 faults 0/10 on 0.10.0, and the whole suite — including a property-test step that had crashed on every attempt on Windows CI — passed on the first attempt on Windows, Linux and macOS runners.

I haven't bisected, so I can't say which change between 0.10.0-pre.4 (which still faulted) and 0.10.0 final is responsible. If a maintainer knows of one that touches connection setup or the amalgamation build, that would confirm it. Otherwise I'm happy for this to be closed as fixed in 0.10.0; if it reappears I'll reopen with the harness output.
