# ImgViewer 威脅模型

最後檢視：2026-07-29

## 保護目標

- 使用者電腦不因開啟圖片而執行任意程式、寫入任意檔案或取得額外權限。
- 惡意或毀損圖片不能造成無界 RAM、CPU、磁碟或 handle 消耗。
- 圖片內容、檔名及本機路徑不離開裝置，也不出現在不必要的記錄中。
- 發布物可追溯到固定來源與依賴，不得無聲替換同版本 ZIP。

## 信任邊界

1. **Windows 與 WebView2**：屬外部平台依賴，必須維持受支援的安全版本。
2. **Vue WebView**：只負責 UI；不能被視為存取控制邊界。選檔與拖放會讓
   UI 暫時取得使用者選擇的路徑，但 Rust snapshot、錯誤與診斷不得回傳
   完整路徑。
3. **Tauri IPC**：只有 `main` 窗口可使用固定 viewer commands、dialog
   open 與 event listen。一次性 render token 取代把檔案路徑當圖片 URL。
4. **Rust catalog / decode**：檔名、目錄項目、magic、metadata、尺寸及
   壓縮資料全部不可信。
5. **原生 codec helper 與 DLL**：libheif、libde265、MSVC runtime 具有
   native code 風險。HEIC／HEIF 已移入私有 helper process；TIFF 與其他
   `image` crate 解碼仍在主程序 worker，是已知殘餘風險。
6. **建置與發布**：npm、Cargo、vcpkg、GitHub Actions、runner 與 ZIP
   stage 都是供應鏈邊界。

## 已有控制

- 無 HTTP、opener、shell、一般 filesystem、遙測或 updater plugin。
- production CSP 僅允許本地資源、Tauri IPC 與 `blob:` 圖片；停用 global
  Tauri API，並啟用 `freezePrototype`。CSP 之外另有原生
  `on_navigation` 白名單：正式版頂層頁面只能留在設定檔實際啟用的
  `http://tauri.localhost`，開發版只額外接受設定檔指定的
  `http://127.0.0.1:1420`。
- `open_path` 只讀取支援圖片；程式不編輯、刪除或在來源資料夾建立檔案。
- UNC、網路與裝置路徑會被拒絕。catalog 建立時及每次開檔前，都由磁碟
  root 到 leaf 依序檢查 drive type 與每個 reparse component，避免先碰
  到已被父 junction 重新導向的 leaf；最終檔案另以 no-follow flag 開啟。
- 輸入上限：256 MiB、單邊 32,768 px、總像素 100,000,000、解碼配置
  512 MiB；額外 metadata 亦有固定上限。同步 catalog 最多檢查 100,000
  個目錄項目並保留 20,000 張圖片。
- 必須正規化成 PNG 的靜態路徑會在配置完整 plane 前，合併估算壓縮來源、
  完整 native plane、RGBA8、轉換列與 PNG output reserve；包含
  RGB32F/RGBA32F TIFF，已知大型緩衝區總和超過 512 MiB 即拒絕。
- GIF／animated WebP 在原始 bytes 交給 WebView2 前完整掃描容器，最多
  10,000 個 frame 與 1,000,000,000 累計 frame pixels；截斷、chunk
  越界與 checked arithmetic 溢位一律 fail closed。
- Rust 驗證 magic 與尺寸；TIFF 只顯示第一頁，HEIC/HEIF 只顯示 primary
  image。
- render token 一次性使用；過期 generation 不能覆蓋目前畫面；Blob URL
  在換圖、錯誤、取消及卸載時回收。
- 解碼排程最多一個執行中與一個可覆蓋 pending 工作。
- 每個同 process 解碼工作帶 30 秒軟期限；超時後的結果不會發布。HEIC／
  HEIF 另由固定同目錄的 helper 處理：啟動時先加入 kill-on-close Job
  Object，限制單一 process、768 MiB 記憶體與每張 30 秒硬期限。
- helper 只接受 duplicated read-only handle、預期長度與 request ID，
  不接受路徑、額外 command line 或輸出位置。固定 binary protocol 限制
  control／render payload，主程式驗證 request ID、PNG signature、IHDR
  與尺寸；timeout、取消、pipe 中斷、crash 或不合法回應都會終止 Job，
  同一輸入不自動重試，下一次選取才 lazy restart。
- 解碼管線只開啟來源一次；檔案大小檢查及有上限的讀取使用同一個唯讀
  handle，magic、probe 與 decode 再從該次取得的 bounded bytes 進行，
  避免檢查後路徑遭替換而改讀另一份內容。
- 來源 handle 在圖片被排入 worker 時就開啟；排隊期間路徑遭替換或刪除
  仍只會讀取原 handle，工作完成後立即釋放。
- window-state 外掛執行前先以 64 KiB 上限讀取及驗證狀態 JSON；無效或
  超限檔會移除，無法安全處理時則拒絕啟動，錯誤記錄不含完整路徑。
- vcpkg baseline、MSVC `v143` overlay triplet、Cargo、pnpm 與 codec 版本固定；release 檢查 DLL
  dependency closure，主 EXE 不得匯入 HEIF codec，helper 必須匯入
  `heif.dll`，兩個 EXE 的 hash 與 protocol version 都寫入 metadata。

## 主要威脅與處理

| 威脅 | 現有處理 | 後續必要工作 |
|---|---|---|
| 壓縮炸彈、巨大靜態尺寸 | 輸入、尺寸、像素上限；mandatory normalization 以 checked arithmetic 預檢已知 aggregate working set；HEIF helper 另有 768 MiB Job cap 與 30 秒硬期限 | TIFF 等同 process codec 的硬隔離與 codec fuzz |
| GIF/WebP animation frame bomb | Rust 完整掃描 GIF image descriptor／WebP `ANMF`，限制 10,000 frames 與 10 億累計 frame pixels；動畫仍保留原始 bytes | WebView 動畫工作不受 Rust soft deadline 約束；helper hard-kill、fixture corpus 與 fuzz 仍是殘餘 DoS 工作 |
| native codec crash、hang 或 OOM | HEIF helper crash／timeout／OOM 會被 Job 終止，主窗口可在下一張重建；固定 native 版本 | 將 TIFF 移入同等 helper；補齊 hang／OOM corpus 與重建壓力測試 |
| 檢查後檔案被替換（TOCTOU） | 每次 open 前重新驗證 drive/reparse；排程時固定單一唯讀 handle，大小檢查與 bounded read 共用該 handle，後續只用該份 bytes | `validate_source_path` 與 `CreateFile` 間仍有主動 parent-junction race；helper/broker 以 component handles／等價 `OBJ_DONT_REPARSE` 策略消除後，再傳 duplicated read-only handle |
| WebView 呼叫未授權能力或導覽到遠端內容 | 主窗口 Capability、嚴格 CSP、原生頂層導覽白名單與負向測試 | 每次安全邊界變更持續擴充測試 |
| render token 猜測或重播 | opaque、一次性、generation 驗證 | 持續測試重播、猜測與資源回收 |
| reparse point、UNC、權限或刪除競態 | root-to-leaf 驗證、開檔前重查與 final no-follow；catalog 後父 junction 置換、權限、刪除與置換為可恢復錯誤 | 檢查到開檔的狹窄主動 race 仍需 component-handle/broker；持續擴充 ACL 與特殊檔案系統 corpus |
| 巨型或毀損 window-state | 64 KiB preflight 在上游 plugin setup 前驗證／移除；記錄不含路徑 | 追蹤上游是否加入原生 read limit |
| 依賴、runner toolset 或 CI Action 遭置換 | 鎖檔、固定版本、MSVC `v143` overlay triplet、app-local native probe、Action SHA、DLL closure、SBOM 與 attestation workflow | installer/updater 前加入 Authenticode 簽章 |
| 診斷洩漏隱私 | 無遙測；UI 不顯示完整路徑 | 診斷匯出預設去識別化，禁止圖片 bytes |

## 不屬安全邊界

- 選檔器與拖放不是檔案存取 sandbox；`open_path` 會接受 WebView 傳入的
  支援圖片路徑。真正的限制是 IPC 白名單、唯讀行為、格式驗證和資源上限。
- CSP 不能保護 Rust 或 native codec 自身的記憶體安全問題。
- HEIF helper 降低 libheif／libde265 拖垮主窗口的風險，但不是 OS sandbox；
  它仍與使用者同權限，安全邊界依賴只讀 handle、無路徑協定與 Job limits。
- TIFF 與 WebView2 動畫尚無可強制終止的 helper；單一惡意輸入仍可能造成
  主 process 或 WebView2 的永久 DoS。
- 無網路與無 updater 降低攻擊面，但使用者仍需自行取得安全更新。

## 變更規則

新增 Capability、Tauri plugin、遠端來源、寫入能力、codec、`unsafe`、
installer 或 updater 前，必須更新本文件、加入負向測試並由維護者明確
核准。PDF、影片、編輯、媒體庫與外部 plugin API 不屬目前產品邊界。
