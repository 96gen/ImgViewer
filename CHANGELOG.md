# Changelog

本專案採用 [Keep a Changelog](https://keepachangelog.com/zh-TW/1.1.0/)
格式；版本號遵循 [Semantic Versioning](https://semver.org/lang/zh-TW/)。

## [Unreleased]

## [0.2.2] - 2026-07-29

### Fixed

- 正式 tag 在 fresh vcpkg 安裝後完整清除 Cargo target，避免候選建置快取中的舊 native artifacts 與新 HEIC DLL 混用，造成測試程序以 `STATUS_DLL_INIT_FAILED` 結束。
- workflow_dispatch 候選建置改走相同的 fresh native/full-clean gate，並禁止帶有 `lib` 前綴的 x265 等未核准 codec DLL。
- HEIC 測試前明確加入相符的 MSVC x64 runtime 搜尋目錄，並先對
  `libde265.dll`／`heif.dll` 執行 loader probe，避免 hosted runner 在
  test harness 啟動前以 `STATUS_DLL_INIT_FAILED` 中止。

### Security

- 更新 `serde_with` 至 3.21.0，修補空序列或 map entry 可能觸發 panic
  的 Dependabot advisory。

## [0.2.1] - 2026-07-29

### Fixed

- 修正 PowerShell 7 將 Cargo 的 `-p` 誤判為模糊 common parameter，導致
  portable tag workflow 在 native dependencies 完成後中止。
- 移除僅執行 audit、未建立 pnpm store 的安全維護工作流程 cache 設定，
  避免所有實質檢查成功後在 cleanup 階段誤報失敗。

## [0.2.0] - 2026-07-27

### Added

- `ViewerSnapshot` 加入 `protocolVersion` 與單調遞增的 `revision`，前端對
  協定漂移、矛盾狀態、過大 binary payload 與過期 revision 採 fail closed。
- 加入可複製發布網址的離線 About 畫面；開啟 About 不會發出網路請求。
- 解碼 worker 支援明確 shutdown／join、panic 隔離與 30 秒 soft deadline。
- 圖片排程時固定單一唯讀 handle；worker 的 deadline 從實際開始解碼才
  計時，不讓舊工作耗掉最新圖片的期限。
- portable 發布加入 SHA-256 manifest、build metadata、CycloneDX SBOM、
  artifact attestation，以及「同一 ZIP 經人工驗證後才發布」的 draft gate。

### Security

- 建立漏洞私下回報、威脅模型、支援與貢獻政策。
- 新增每週 Cargo、npm 與 GitHub Actions 依賴更新。
- 新增 `cargo audit`、`cargo deny` 與 production `pnpm audit` 工作流程。
- CI 第三方 Action 改為固定且已驗證的完整 commit SHA。
- 解碼開檔、大小檢查及 bounded read 共用單一唯讀 handle；其後只處理該
  次取得的 bytes，避免來源路徑置換造成 probe/decode 不一致。
- 拒絕 UNC、裝置路徑與 reparse point；同步 catalog 加入 100,000 個目錄
  項目及 20,000 張圖片上限。
- 在 window-state 外掛 setup 前，以 64 KiB 上限驗證不可信狀態檔，並從
  診斷訊息移除完整本機路徑。
- GIF／animated WebP 在交給 WebView2 前完整掃描 frame 結構，限制
  10,000 frames 與 1,000,000,000 累計 frame pixels；截斷、chunk
  越界與超限回傳穩定、無路徑的錯誤參數。
- Mandatory PNG normalization 在完整配置前預檢 source/native、RGBA8、
  色彩轉換列與 PNG output 的 aggregate working set；TIFF 依完整
  `ColorType` 計入 32-bit float plane。
- 每次開檔前由 root 到 leaf 重查 drive 與 reparse component，final file
  使用 no-follow；移除 catalog 內會跟隨路徑的 `canonicalize` fallback。
- 修補 `rand`、`quick-xml` 與 `time` 的已知 RustSec advisory，更新到
  `rand 0.9.3`、`plist 1.10.0`、`quick-xml 0.41.0`、`time 0.3.54`。

### Changed

- 專案版本提升為 0.2.0；Windows 11 為正式安全驗收平台，Windows 10
  22H2 改為 best-effort。
- Rust 預設禁止 `unsafe`；僅 Win32、Windows 自然排序及 codec FFI
  adapter 可局部啟用，且每一段都需有 `SAFETY` 說明。

## [0.1.2] - 2026-07-22

### Fixed

- 切換有效圖片時保留舊畫面，候選圖片完成預解碼後才原子換圖，降低閃爍。
- 修正縮放按鈕、Fit 與 100% 操作在 pointer capture 下失效的情況。

### Performance

- 限制同時保留目前圖片與單一候選圖片，回收過期 Blob URL。
- 加入正式 binary 的 RAM 循環 smoke 與回收門檻。

### Security

- 維持圖片專用、離線、唯讀與最小 Tauri Capability。
- 保留輸入大小、尺寸、總像素、解碼配置及色彩 metadata 限制。
