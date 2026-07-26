# Roadmap

Roadmap 是安全門檻，不是發布日期承諾。只有目前正在維護的版本列為
active；未排程項目不代表承諾。

## 0.2.x — 安全與可維護性基礎

- [x] 保存可回退的 0.1.2 Git 基線與 tag。
- [x] 版本化 IPC snapshot、revision 與結構化錯誤參數。
- [x] worker shutdown、panic 隔離與可測試的 deadline policy。
- [x] Capability、CSP、依賴與 binary payload 契約測試。
- [x] 選取時固定唯讀 handle，拒絕 UNC／reparse point，限制同步 catalog
  與 window-state 輸入。
- [x] GIF／WebP 完整掃描 frame 結構，限制 10,000 frames 與 10 億累計
  frame pixels；超限、截斷與 chunk 越界 fail closed。
- [x] Mandatory PNG normalization 預檢已知 aggregate working set，涵蓋
  compressed source、native／float plane、RGBA8 與 PNG output reserve。
- [x] Security、support、contribution、threat model、ADR 與維護節奏。
- [x] Dependabot、RustSec、cargo-deny、production pnpm audit。
- [x] Release checksum、build metadata、SBOM 與 provenance workflow。

## 0.3–0.5 — 原生解碼隔離

- [ ] 將 TIFF／HEIF 解碼移至獨立 helper process。
- [ ] 以 Windows Job Object 限制預設 768 MiB RAM 與 30 秒硬期限。
- [ ] 只傳 duplicated read-only handle，不傳任意輸出路徑或 command。
- [ ] helper／broker 以逐層 component handle 或等價 no-reparse 核心語意
  消除 `validate_source_path` 到 `CreateFile` 間的 parent-junction race。
- [ ] catalog 背景化，涵蓋大型本機資料夾、進度與取消；UNC／reparse point
  維持 fail closed。
- [ ] 增加格式 probe、metadata 與 codec corpus fuzz。
- [ ] 對 WebView2 動畫解碼加入可強制終止的隔離策略；既有結構上限不能
  單獨中止 codec hang。
- [ ] 提供使用者主動匯出、預設去識別化的本機診斷。

## 1.0.0 — 穩定版門檻

- [ ] 六類格式、動畫、惡意 corpus、RAM、競態、無閃切換與窗口幾何
  全部通過 release gate。
- [ ] 乾淨 Windows 11 VM 能驗證 release checksum、SBOM 與 attestation。
- [ ] installer 或 updater 若存在，ImgViewer 自有 EXE 必須先有持續可用的
  Authenticode 簽章；否則 portable ZIP 保持唯一發行方式。

PDF、影片、編輯、媒體庫、雲端、遙測與外部 plugin API不在核心 Roadmap。
