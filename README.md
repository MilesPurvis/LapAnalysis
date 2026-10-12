# LapAnalysis

Download sessions from an AiM Solo 2 DL over Wi-Fi, without RaceStudio 3, and browse them
in a local web app: laps, a speed-coloured GPS map, and synced channel charts.

The app lives in [`app/`](app/README.md). [`HOW_IT_WORKS.md`](HOW_IT_WORKS.md) explains the
logger's Wi-Fi protocol and how the session files' signals are decoded. Downloaded sessions go in `library/` at the repo
root, which the app creates on first run (git-ignored).

```sh
cd app && cargo build --release && ./target/release/lapanalysis   # http://127.0.0.1:8080
```

## Versions

Commits on `main` follow [Conventional Commits](https://www.conventionalcommits.org), and
[release-please](https://github.com/googleapis/release-please) turns them into versions:

| Commit | Bump |
|---|---|
| `fix: …` | patch (0.1.0 → 0.1.1) |
| `feat: …` | minor (0.1.0 → 0.2.0) |
| `feat!: …` or a `BREAKING CHANGE:` footer | major (0.1.0 → 1.0.0) |
| `docs:`, `ci:`, `chore:`, `refactor:`, `test:` | no release |

It keeps a `chore(main): release x.y.z` PR open with the version bump and
[`app/CHANGELOG.md`](app/CHANGELOG.md). Merge it when you want a release; that tags `vx.y.z`
and publishes a GitHub Release. Mention the issue in the commit (`feat: lap overlay (#12)`)
so each feature request shows which version it shipped in. The running version is in the
app header and `lapanalysis --version`.
