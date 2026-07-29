# ImgViewer

ImgViewer 是 Windows x64 專用的免安裝圖片瀏覽器。介面以 Vue 3 製作，檔案列舉、格式驗證、解碼與競態控制由 Rust/Tauri 2 處理。支援 GIF、JPG/JPEG、PNG、TIFF、WebP、HEIC/HEIF；GIF 與 animated WebP 保留動畫，TIFF 顯示第一頁，HEIC/HEIF 顯示 primary image。16-bit PNG、TIFF、10/12/16-bit HEIC/HEIF，以及帶非 sRGB RGB ICC、PNG cICP 或 cHRM/gAMA 的靜態圖片，會先做色彩轉換、最後才量化成帶 sRGB ICC 的 8-bit PNG。

## 使用需求

- Windows 11 x64 是正式支援與安全驗收平台。Windows 10 22H2 x64
  僅 best-effort，不阻擋 WebView2、Tauri 或 codec 的必要安全更新。
- Microsoft Edge WebView2 Evergreen Runtime。支援版本的 Windows 通常已安裝；若系統提示缺少 runtime，請使用 Microsoft 的 [WebView2 Runtime 下載頁](https://developer.microsoft.com/microsoft-edge/webview2/consumer/) 安裝 Evergreen Runtime。
- 不需要 Windows HEIF Image Extensions；ZIP 已包含 `libheif` 與 `libde265`。

解壓縮 `ImgViewer-{version}-windows-x64.zip` 後，直接執行資料夾內的
`ImgViewer.exe`。請保留 `ImgViewer.CodecHelper.exe`、DLL 與授權文件在
同一資料夾；helper 缺少或被移動時，其他格式仍可使用，但 HEIC／HEIF 會
顯示可恢復錯誤。

## 操作

- 「開啟圖片」或 `Ctrl+O`：選擇圖片。
- 將圖片拖進窗口：開啟該圖片。
- `ImgViewer.exe "C:\path\photo.jpg"`：從命令列開啟；再次啟動會交給現有窗口並將它帶到前景。
- `←` / `→` 或畫面左右按鈕：上一張 / 下一張；到首尾停止，不循環。
- 滾輪：以游標位置為中心縮放，範圍 10%–1600%。
- 拖曳圖片：平移。
- `Ctrl+0`：Fit；`Ctrl+1`：100%。

每次開圖會建立該資料夾第一層的固定清單，副檔名不分大小寫並採 Windows 自然排序。切換圖片會回到 Fit。毀損、遭刪除或超過安全限制的檔案顯示內嵌錯誤，仍可繼續切換。

切換有效圖片時，舊圖會保持顯示，直到下一張在背景完成預解碼才一次換上；載入期間只顯示角落提示，不再讓中央圖片區閃成空白或 spinner。

圖片載入不會調整原生窗口。使用者設定的窗口位置、尺寸與最大化狀態會保存；啟動時先還原並校正到目前螢幕工作區，再顯示窗口。

## 效能與記憶體

- 一般 JPG、8-bit PNG、GIF 與 WebP 只在 Rust 端驗證 magic、尺寸、方向及必要色彩 metadata，原始壓縮 bytes 直接交給 WebView2，避免預先建立第二份完整像素平面。
- 必須轉成 8-bit sRGB PNG 的 TIFF、HEIC/HEIF、高位元或廣色域圖片，會在配置完整像素平面前，以 checked arithmetic 合併計算壓縮來源、native plane（含 32-bit float TIFF）、RGBA8、色彩轉換列與保守 PNG 輸出 reserve；已知大型緩衝區總和超過 512 MiB 即拒絕。實際流程仍會在 PNG 編碼前提早釋放原始輸入與 native 解碼平面。
- binary IPC 讀取一次只允許一個在途；快速連續切圖時，尚未開始的舊 generation 會被最後目標覆蓋。候選 Blob 會先由 WebView2 預解碼，確認仍是最新 generation 後才原子換圖；舊 Blob 會保留到下一個畫面週期再撤銷。
- 無閃切換期間最多短暫同時保留一張目前顯示圖與一張候選圖；過期候選會取消並立即回收，不做多張預載或長時間 crossfade。
- 拖曳平移合併到每個 animation frame 更新一次；重複但尺寸未變的 ResizeObserver 通知不觸發重算。

## 安全與隱私

ImgViewer 不含網路、遙測、更新器、shell 或一般檔案系統 API。WebView 會接收選檔或拖放產生的路徑，再交給只接受支援圖片格式的 `open_path` command；command 不驗證該路徑一定來自選檔器，因此這不是檔案存取 sandbox。WebView 沒有一般目錄列舉／讀寫 API，圖片內容則只透過一次性 binary render token 回傳。CSP 只允許本地內容、Tauri IPC 與圖片 `blob:` URL；原生導覽 hook 另會拒絕所有不在固定本地 origin 白名單內的頂層頁面。

固定限制為：輸入檔 256 MiB、單邊 32,768 px、總像素 100,000,000、已知解碼工作集 512 MiB；GIF／animated WebP 另限制 10,000 個 frame 與 1,000,000,000 累計 frame pixels。動畫原始 bytes 交給 WebView2 前，Rust 會先完整掃描 GIF image descriptor 或 WebP `ANMF` 結構，截斷、越界或超限都顯示可恢復錯誤。第一版不遞迴掃描、不監看資料夾，也不提供編輯、儲存、刪除、縮圖列、檔案關聯或安裝程式。

為維持真正離線，UNC、網路、裝置路徑與 reparse point 會被拒絕。同步資料夾
列舉最多檢查 100,000 個項目並保留 20,000 張圖片；超限會顯示可恢復錯誤。
每次 navigation 開檔前會由磁碟 root 到檔案重新檢查 drive type 與 reparse
component，最後檔案再以 no-follow flag 開啟。這可拒絕 catalog 建立後已被換成
junction 的父資料夾，但檢查與 `CreateFile` 仍是兩個 Win32 操作；能主動搶在
兩者之間反覆切換 reparse point 的本機攻擊者仍是已知競態，完整 component
handle／broker 隔離列在 Roadmap。
窗口狀態 JSON 在外掛載入前也有 64 KiB 上限。GIF／animated WebP 仍由
WebView2 播放；結構型 frame／累計像素限制可先拒絕 frame bomb，但目前沒有
helper process 能硬中止 WebView2 的動畫解碼，因此仍不宣稱已完全隔離
animation CPU／codec hang DoS。

HEIC／HEIF 解碼已移入固定同目錄的私有 helper process。主程式只把已開啟
的 read-only handle 複製給 helper，不傳來源路徑、任意 command line 或
輸出路徑；helper 啟動後先受 Windows Job Object 約束，限制單一 process、
768 MiB 記憶體與每張 30 秒硬期限。timeout、取消、pipe 中斷、codec crash
或不合法回應都會終止該 Job；同一張不自動重試，下一次選取才建立乾淨
helper。

TIFF 與其他同 process 解碼仍只有 30 秒 soft deadline：超時返回的結果不會
覆蓋畫面，但卡死的 codec thread 無法安全強制中止。TIFF 隔離、parent
junction race 與 WebView2 動畫解碼隔離仍列在 [Roadmap](ROADMAP.md)，
並記錄於 [威脅模型](THREAT_MODEL.md)。漏洞請依
[安全政策](SECURITY.md) 私下回報；維護頻率與支援邊界分別見
[維護節奏](MAINTENANCE.md)、[支援政策](SUPPORT.md)。

HDR 的 PQ/HLG transfer 與廣色域原色會轉進有限的 8-bit sRGB 顯示範圍；超出 SDR／sRGB 範圍的值會截斷，第一版不輸出 HDR，也不提供可調整的感知式 tone mapping。

色彩 metadata 的第一版邊界：PNG cICP 只接受 full-range RGB identity matrix，narrow-range 或 YCbCr cICP 會顯示可恢復的 `unsupported_color_profile`，不猜測轉換。HEIF 優先使用 container ICC，其次使用 libheif 暴露的 NCLX；若檔案只有 codec bitstream VUI、沒有 `colr` ICC/NCLX，`libheif-rs 2.7.0` 不提供該 VUI profile，會採一般 sRGB fallback。

## 開發

Windows 建置環境需要：

- Node.js 24.18.0（見 `.node-version`）與 `package.json` 指定的 pnpm
  版本。
- Rust 1.88 以上的 MSVC toolchain（`image 0.25.10` 的最低需求）。
- Visual Studio 2022 Build Tools，包含 Desktop development with C++、MSVC x64 tools、Windows SDK 與 VC++ Redistributable files。
- Windows PowerShell 5.1 或 PowerShell 7，以及 Git。

一般測試與前端開發不需要安裝 HEIC native library：

```powershell
pnpm install --frozen-lockfile
pnpm test
pnpm build
cargo fmt --manifest-path .\src-tauri\Cargo.toml --all -- --check
cargo clippy --locked --manifest-path .\src-tauri\Cargo.toml --workspace --all-targets --no-default-features -- -D warnings
cargo test --locked --manifest-path .\src-tauri\Cargo.toml --workspace --no-default-features
```

原生窗口 smoke 不使用 WDIO、WebDriver 或測試外掛；它直接啟動正式 binary，以 Windows UI Automation 找到已顯示的圖片，再用 Win32 讀取原生窗口 rect：

```powershell
$version = (Get-Content .\src-tauri\tauri.conf.json -Raw | ConvertFrom-Json).version
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-portable.ps1 -KeepStage -SkipNativeSmoke
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\smoke-native.ps1 -Executable ".\release\ImgViewer-$version-windows-x64\ImgViewer.exe"
```

此 smoke 覆蓋 CLI 開圖、單次方向鍵自然排序、首尾停止、無等待 UIA
連按的快速 latest-wins、single-instance handoff、切圖過程 UIA 圖片不中斷，
以及每次操作前後窗口 rect 完全相同。快速 Arrow key 的同步事件與反序
response 另由 Vitest 固定重現。專案不含 WDIO／WebDriver dependency 或
自動化 command；完整 PASS anchors 見 [scripts/VERIFYING.md](scripts/VERIFYING.md)。

正式 EXE 的 RAM smoke 同樣不使用 WDIO／WebDriver。它動態產生測試圖，循環走 single-instance 開圖，以 UI Automation 等待圖片，再統計 ImgViewer 與所有 WebView2 子程序的 peak／retained private bytes、working set、斜率與 p95 載入時間：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\smoke-memory.ps1 `
  -Executable ".\release\ImgViewer-$version-windows-x64\ImgViewer.exe"
```

此量測需在互動式 Windows 桌面執行；CSV 預設保留在系統 TEMP，也可用 `-OutputCsv` 指定路徑。跨機器不應直接比較絕對 RAM，before／after 必須使用同一台機器、相同 WebView2 runtime、圖片尺寸與參數。

0.1.1 的固定環境 before／after 數據，以及 0.1.2 無閃切圖的 100／160 輪 RAM 回收結果與解讀限制，見 [PERFORMANCE.md](PERFORMANCE.md)。

要測試 HEIC helper，先佈署固定版 vcpkg 依賴，再建置 helper。Tauri 主
process 不得啟用 HEIC feature：

```powershell
$nativeBin = powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\install-native-deps.ps1 | Select-Object -Last 1
cargo clean --manifest-path .\src-tauri\Cargo.toml --package libheif-sys
$env:VCPKG_ROOT = "$PWD\.tools\vcpkg"
$env:VCPKG_DEFAULT_TRIPLET = "x64-windows"
$env:VCPKG_DEFAULT_HOST_TRIPLET = "x64-windows"
$env:VCPKGRS_DYNAMIC = "1"
$env:VCPKGRS_TRIPLET = "x64-windows"
$env:PATH = "$nativeBin;$env:PATH"
cargo build --locked --manifest-path .\src-tauri\Cargo.toml --package imgviewer-codec-helper --features heic
Copy-Item .\src-tauri\target\debug\imgviewer-codec-helper.exe .\src-tauri\target\debug\ImgViewer.CodecHelper.exe -Force
pnpm run tauri dev
```

真正的 broker／Job Object／duplicated-handle process 測試可直接執行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\test-codec-helper-process.ps1
```

`.tools/vcpkg` 固定為 tag `2026.05.25`。這是 annotated tag：tag object 是 `baddcee32f29086c2c1c1f002df5078e371f7934`，實際 checkout 與 `builtin-baseline` 必須使用 peeled commit `d015e31e90838a4c9dfa3eed45979bc70d9357fc`。專案內的 `vcpkg-overlays/ports/libheif` 保留該版本 port，僅在編譯時停用 libheif runtime plugin loading；HEIC 仍由內建註冊的 libde265 decoder 解碼，程式不會掃描外部 codec DLL。

## 建立 portable ZIP

在 x64 Windows 的 Developer PowerShell 執行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-portable.ps1
```

腳本會依序：

1. 驗證並 bootstrap 固定版 vcpkg。
2. 以 dynamic `x64-windows` 安裝 `libheif[core]`，overlay triplet 固定 MSVC `v143`，避免 hosted runner 新增 Visual Studio toolset 後悄悄改變 native ABI；`default-features: false` 排除 x265 與非必要 codec，overlay port 關閉 runtime plugin loading。
3. 對全 Rust workspace 跑無 HEIC 測試，再對 codec core／helper 跑
   HEIC 測試；Tauri 主程式以 `--no-bundle` 且不含 HEIC 建置，helper
   另以 `--features heic` 建置。
4. 將 `ImgViewer.exe` 與 `ImgViewer.CodecHelper.exe` 一起放入 stage，
   用 `dumpbin /DEPENDENTS` 遞迴收集 vcpkg DLL 與必要 MSVC runtime。
5. 驗證主 EXE 不匯入 `heif.dll`／`libde265.dll`、helper 必須匯入
   `heif.dll`；移除外部搜尋路徑後再檢查 dependency closure。
6. 對 stage binary 執行無 WebDriver 的 native smoke。
7. 加入 README、MIT/LGPL 授權、來源版本通知與 schema v2
   `BUILD_METADATA.json`（含兩個 EXE 的 role、protocol version 與 SHA-256），
   輸出 ZIP、SHA-256 manifest 與 build metadata sidecar。

GitHub 的 tag workflow 只建置一次並建立 draft release；CI 另產生並補強
CycloneDX SBOM，且對 ZIP、checksum、metadata 與 SBOM 建立 artifact
attestation。互動式 Windows 11 測試機必須下載同一份 ZIP、核對 digest
並完成 packaged smoke，之後才可用該 digest 將 draft 升為正式 release；
promotion 不會重新建置或替換資產。完整程序見
[發布驗證指南](docs/release/README.md)。

若測試已由同一個乾淨 commit 的 CI job 通過，可使用 `-SkipChecks`；它不會略過 native dependency 或 ZIP closure 驗證。`-SkipNativeSmoke` 只供沒有互動式桌面的 CI，或供稍後依上方命令手動執行同一個 smoke。完整的自動與人工驗證步驟見 [scripts/VERIFYING.md](scripts/VERIFYING.md)。

## 授權

ImgViewer 使用 [MIT License](LICENSE)。第三方套件、LGPL 動態函式庫與來源取得方式見 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
