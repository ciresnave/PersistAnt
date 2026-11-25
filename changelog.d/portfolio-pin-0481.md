kind: Changed: the portfolio workflow pin in ci.yml moves to coderipper 467fd25 (0.48.1)
---
The `portfolio` job in `.github/workflows/ci.yml` now calls coderipper's `portfolio.yml` at `467fd259d6d062cba4c5683d310f1a6f2b66e763` (coderipper#375, which installs 0.48.1) in place of `dc45e36` (0.47.21). CI configuration only: no code, test, dependency or version change. The PR's own `portfolio` check is the evidence that the new pin runs.
