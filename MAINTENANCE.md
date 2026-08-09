# 維護節奏

ImgViewer 的成功標準不是每月增加功能，而是即使一年沒有新功能，仍能在
支援中的 Windows 上由乾淨來源建置、驗證、發布與安全地開圖。

## 每次變更

- 執行 Vitest、Rust tests、frontend build、`cargo fmt` 與 Clippy。
- 若改到 Capability、CSP、plugin、codec、`unsafe` 或輸入限制，更新
  [THREAT_MODEL.md](THREAT_MODEL.md) 並加入負向測試。
- 審查 `Cargo.lock`、`pnpm-lock.yaml`、vcpkg overlay 與 GitHub Action
  SHA 的變化；不得用 advisory ignore 取代可行的安全更新。
- 新增 fixture 時記錄來源、授權與去識別化方式，禁止提交私人圖片。

## 每週自動

- Dependabot 分別檢查 npm、Cargo 與 GitHub Actions；Tauri、Vue、前端工具、
  codec 與 Rust core 使用不同更新群組，避免無關風險在同一個 PR 混升。
- Security maintenance workflow 執行 RustSec、cargo-deny 的 advisory、
  duplicate dependency、license、source policy，以及 production pnpm
  audit。
- 警報只有在確認不影響 Windows production graph，且文件記錄原因與
  到期日後才能暫時例外；預設為 fail closed。

## 每月人工（約 30–60 分鐘）

- 檢查 Tauri、WebView2、RustSec、`image`、libheif、libde265 與 vcpkg
  上游安全公告。
- 檢查 GitHub private vulnerability reports、Dependabot 與失敗的
  scheduled workflows。
- Critical／High 問題依 [SECURITY.md](SECURITY.md) 止血；不要等季度版本。

## 每季

- UI、Rust 與 native codec 分批更新，每批獨立執行完整格式、RAM、
  快速切圖競態及 packaged UI Automation。
- 視實際修正發布 maintenance release；不為了版本節奏強迫增加功能。
- 在互動式 Windows 11 執行相同 ZIP digest 的 release gate。

## 每年

- 在目前維護的 Windows 11 實機建立全新 clone，重新 bootstrap 專案內工具，
  從零建置並驗證 Git tag、checksum、SBOM 與 provenance；不要求 VM 或
  乾淨作業系統映像。
- 建立離線 Git bundle，測試備份確實可還原。
- 盤點 repository 權限、branch/tag protection、簽章金鑰、支援中的
  Windows/WebView2，以及第三方授權。
- 重讀威脅模型並演練一次「codec 出現 Critical CVE」的停用與補版流程。
