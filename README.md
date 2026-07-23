# ImgViewer

ImgViewer 是 Windows x64 專用的免安裝圖片瀏覽器。介面以 Vue 3 製作，檔案列舉、格式驗證、解碼與競態控制由 Rust/Tauri 2 處理。支援 GIF、JPG/JPEG、PNG、TIFF、WebP、HEIC/HEIF；GIF 與 animated WebP 保留動畫，TIFF 顯示第一頁，HEIC/HEIF 顯示 primary image。16-bit PNG、TIFF、10/12/16-bit HEIC/HEIF，以及帶非 sRGB RGB ICC、PNG cICP 或 cHRM/gAMA 的靜態圖片，會先做色彩轉換、最後才量化成帶 sRGB ICC 的 8-bit PNG。

## 使用需求

- Windows 10 22H2 x64 或 Windows 11 x64。
- Microsoft Edge WebView2 Evergreen Runtime。支援版本的 Windows 通常已安裝；若系統提示缺少 runtime，請使用 Microsoft 的 [WebView2 Runtime 下載頁](https://developer.microsoft.com/microsoft-edge/webview2/consumer/) 安裝 Evergreen Runtime。
- 不需要 Windows HEIF Image Extensions；ZIP 已包含 `libheif` 與 `libde265`。

解壓縮 `ImgViewer-{version}-windows-x64.zip` 後，直接執行資料夾內的 `ImgViewer.exe`。請保留 EXE、DLL、授權文件在同一資料夾。

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
- 必須轉成 8-bit sRGB PNG 的 TIFF、HEIC/HEIF、高位元或廣色域圖片，會在 PNG 編碼前提早釋放原始輸入與 native 解碼平面；色彩轉換重用單一 scanline buffer。
- binary IPC 讀取一次只允許一個在途；快速連續切圖時，尚未開始的舊 generation 會被最後目標覆蓋。候選 Blob 會先由 WebView2 預解碼，確認仍是最新 generation 後才原子換圖；舊 Blob 會保留到下一個畫面週期再撤銷。
- 無閃切換期間最多短暫同時保留一張目前顯示圖與一張候選圖；過期候選會取消並立即回收，不做多張預載或長時間 crossfade。
- 拖曳平移合併到每個 animation frame 更新一次；重複但尺寸未變的 ResizeObserver 通知不觸發重算。

## 安全與隱私

ImgViewer 不含網路、遙測、更新器、shell 或一般檔案系統 API。WebView 會接收選檔或拖放產生的路徑，再交給只接受支援圖片格式的 `open_path` command；command 不驗證該路徑一定來自選檔器，因此這不是檔案存取 sandbox。WebView 沒有一般目錄列舉／讀寫 API，圖片內容則只透過一次性 binary render token 回傳。CSP 只允許本地內容、Tauri IPC 與圖片 `blob:` URL。

固定限制為：輸入檔 256 MiB、單邊 32,768 px、總像素 100,000,000、解碼配置 512 MiB。第一版不遞迴掃描、不監看資料夾，也不提供編輯、儲存、刪除、縮圖列、檔案關聯或安裝程式。

HDR 的 PQ/HLG transfer 與廣色域原色會轉進有限的 8-bit sRGB 顯示範圍；超出 SDR／sRGB 範圍的值會截斷，第一版不輸出 HDR，也不提供可調整的感知式 tone mapping。

色彩 metadata 的第一版邊界：PNG cICP 只接受 full-range RGB identity matrix，narrow-range 或 YCbCr cICP 會顯示可恢復的 `unsupported_color_profile`，不猜測轉換。HEIF 優先使用 container ICC，其次使用 libheif 暴露的 NCLX；若檔案只有 codec bitstream VUI、沒有 `colr` ICC/NCLX，`libheif-rs 2.7.0` 不提供該 VUI profile，會採一般 sRGB fallback。

## 開發

Windows 建置環境需要：

- Node.js 20.19 以上與 `package.json` 指定的 pnpm 版本。
- Rust 1.88 以上的 MSVC toolchain（`image 0.25.10` 的最低需求）。
- Visual Studio 2022 Build Tools，包含 Desktop development with C++、MSVC x64 tools、Windows SDK 與 VC++ Redistributable files。
- Windows PowerShell 5.1 或 PowerShell 7，以及 Git。

一般測試與前端開發不需要安裝 HEIC native library：

```powershell
pnpm install --frozen-lockfile
pnpm test
pnpm build
cargo fmt --manifest-path .\src-tauri\Cargo.toml --all -- --check
cargo clippy --manifest-path .\src-tauri\Cargo.toml --all-targets --no-default-features -- -D warnings
cargo test --manifest-path .\src-tauri\Cargo.toml --no-default-features
```

原生窗口 smoke 不使用 WDIO、WebDriver 或測試外掛；它直接啟動正式 binary，以 Windows UI Automation 找到已顯示的圖片，再用 Win32 讀取原生窗口 rect：

```powershell
$version = (Get-Content .\src-tauri\tauri.conf.json -Raw | ConvertFrom-Json).version
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-portable.ps1 -KeepStage -SkipNativeSmoke
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\smoke-native.ps1 -Executable ".\release\ImgViewer-$version-windows-x64\ImgViewer.exe"
```

此 smoke 覆蓋 CLI 開圖、自然排序、首尾停止、快速方向鍵、single-instance handoff、切圖過程 UIA 圖片不中斷，以及每次操作前後窗口 rect 完全相同。專案不含 WDIO／WebDriver dependency 或自動化 command；完整 PASS anchors 見 [scripts/VERIFYING.md](scripts/VERIFYING.md)。

正式 EXE 的 RAM smoke 同樣不使用 WDIO／WebDriver。它動態產生測試圖，循環走 single-instance 開圖，以 UI Automation 等待圖片，再統計 ImgViewer 與所有 WebView2 子程序的 peak／retained private bytes、working set、斜率與 p95 載入時間：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\smoke-memory.ps1 `
  -Executable ".\release\ImgViewer-$version-windows-x64\ImgViewer.exe"
```

此量測需在互動式 Windows 桌面執行；CSV 預設保留在系統 TEMP，也可用 `-OutputCsv` 指定路徑。跨機器不應直接比較絕對 RAM，before／after 必須使用同一台機器、相同 WebView2 runtime、圖片尺寸與參數。

0.1.1 的固定環境 before／after 數據，以及 0.1.2 無閃切圖的 100／160 輪 RAM 回收結果與解讀限制，見 [PERFORMANCE.md](PERFORMANCE.md)。

要在開發模式測試 HEIC，先佈署固定版 vcpkg 依賴，再啟用 `heic` feature：

```powershell
$nativeBin = powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\install-native-deps.ps1 | Select-Object -Last 1
cargo clean --manifest-path .\src-tauri\Cargo.toml -p libheif-sys
$env:VCPKG_ROOT = "$PWD\.tools\vcpkg"
$env:VCPKGRS_DYNAMIC = "1"
$env:VCPKGRS_TRIPLET = "x64-windows"
$env:PATH = "$nativeBin;$env:PATH"
pnpm exec tauri dev --features heic
```

`.tools/vcpkg` 固定為 tag `2026.05.25`。這是 annotated tag：tag object 是 `baddcee32f29086c2c1c1f002df5078e371f7934`，實際 checkout 與 `builtin-baseline` 必須使用 peeled commit `d015e31e90838a4c9dfa3eed45979bc70d9357fc`。專案內的 `vcpkg-overlays/ports/libheif` 保留該版本 port，僅在編譯時停用 libheif runtime plugin loading；HEIC 仍由內建註冊的 libde265 decoder 解碼，程式不會掃描外部 codec DLL。

## 建立 portable ZIP

在 x64 Windows 的 Developer PowerShell 執行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-portable.ps1
```

腳本會依序：

1. 驗證並 bootstrap 固定版 vcpkg。
2. 以 dynamic `x64-windows` 安裝 `libheif[core]`；`default-features: false` 排除 x265 與非必要 codec，overlay 關閉 runtime plugin loading。
3. 執行前端、Rust 測試，然後執行 Tauri `--no-bundle --features heic` release build。
4. 用 `dumpbin /DEPENDENTS` 遞迴收集 vcpkg DLL 與必要 MSVC runtime。
5. 移除外部搜尋路徑後再檢查 dependency closure 與禁止 codec 清單；任何未封裝的非系統 DLL 都會使發佈失敗。
6. 對 stage binary 執行無 WebDriver 的 native smoke。
7. 加入 README、MIT/LGPL 授權與來源版本通知，輸出 `release/ImgViewer-{version}-windows-x64.zip`。

若測試已由同一個乾淨 commit 的 CI job 通過，可使用 `-SkipChecks`；它不會略過 native dependency 或 ZIP closure 驗證。`-SkipNativeSmoke` 只供沒有互動式桌面的 CI，或供稍後依上方命令手動執行同一個 smoke。完整的自動與人工驗證步驟見 [scripts/VERIFYING.md](scripts/VERIFYING.md)。

## 授權

ImgViewer 使用 [MIT License](LICENSE)。第三方套件、LGPL 動態函式庫與來源取得方式見 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
