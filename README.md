# LapAnalysis

Download sessions from an AiM Solo 2 DL over Wi-Fi, without RaceStudio 3, and browse them
in a local web app: laps, a speed-coloured GPS map, and synced channel charts.

The app lives in [`app/`](app/README.md). [`HOW_IT_WORKS.md`](HOW_IT_WORKS.md) explains the
logger's Wi-Fi protocol and how the session files' signals are decoded. Downloaded sessions go in `library/` at the repo
root, which the app creates on first run (git-ignored).

```sh
cd app && cargo build --release && ./target/release/lapanalysis   # http://127.0.0.1:8080
```
