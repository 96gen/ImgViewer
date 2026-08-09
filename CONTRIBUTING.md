# 貢獻指南

感謝協助改善 ImgViewer。專案維持 Windows、離線、唯讀、圖片專用；
請先閱讀 [THREAT_MODEL.md](THREAT_MODEL.md) 與 `docs/adr/`。

## 開發原則

- 不加入 PDF、網路、遙測、自動更新、shell、一般 filesystem API 或外部
  plugin API。
- 不得因圖片尺寸、格式或錯誤改變原生窗口的位置與大小。
- 圖片、metadata、路徑、資料夾內容與狀態檔一律視為不可信。
- 新增 Tauri command、Capability、CSP、plugin、原生 DLL 或 `unsafe`
  必須附威脅分析與負向測試。每個 `unsafe` block 必須有 `SAFETY` 說明。
- 測試 fixture 應為自行產生或可再散布的去識別化資料，禁止提交私人照片。
- 不使用 WDIO、`wdio.js`、WebDriver 或正式版測試 plugin。

## 驗證

在 Windows 執行：

```powershell
pnpm install --frozen-lockfile
pnpm test
pnpm build
cargo fmt --manifest-path .\src-tauri\Cargo.toml --all -- --check
cargo clippy --locked --manifest-path .\src-tauri\Cargo.toml --workspace --all-targets --no-default-features -- -D warnings
cargo test --locked --manifest-path .\src-tauri\Cargo.toml --workspace --no-default-features
```

涉及 UI、窗口、RAM、原生 codec 或發布內容時，另依
[`scripts/VERIFYING.md`](scripts/VERIFYING.md) 執行 packaged smoke。

## Pull request

- 每個 PR 聚焦一項可審查的變更，說明症狀、根因、修正與驗證證據。
- 行為或安全政策變更需更新文件、測試與 `CHANGELOG.md`。
- 依賴更新必須提交 `pnpm-lock.yaml` 或 `src-tauri/Cargo.lock` 的必要差異；
  UI、Rust 與 codec 更新應分開。
- 不得手動修改或原地替換已發布的同版本 ZIP。
- CI 綠燈不取代互動式 Windows 驗收；無法執行的項目必須明確標示。
