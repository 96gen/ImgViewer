# Portable release integrity

ImgViewer 的正式產物只由 tag workflow 建置一次。Tag push 會產生：

- `ImgViewer-{version}-windows-x64.zip`
- 同名 `.zip.sha256`（標準 `sha256sum` 格式）
- `.build.json`（commit、dirty state、工具鏈、vcpkg、codec 與 MSVC runtime）
- `.cdx.json`（CycloneDX 1.6 SBOM）
- GitHub artifact attestation

工作流程先建立 **draft GitHub Release**。Draft 中的資產不會在 UI 驗收後重新建置或
覆寫；人工 promotion 只會驗證同一個 ZIP 的 digest，然後把既有 draft 公開。

## 本機候選版

一般開發環境可在 dirty tree 執行，狀態會如實寫入 metadata：

```powershell
.\scripts\build-portable.ps1 -SkipNativeSmoke
```

正式模式要求七個版本來源相同、Git tree 完全乾淨、`v{version}` tag 指向
HEAD，且 GitHub tag ref 與 checkout commit 一致：

```powershell
$version = (Get-Content .\src-tauri\tauri.conf.json -Raw | ConvertFrom-Json).version
.\scripts\build-portable.ps1 -SkipNativeSmoke -ReleaseMode -ExpectedTag "v$version"
```

正式模式還會清除 vcpkg 的 ignored `installed`、`packages`、`buildtrees`，
停用 binary cache，並驗證固定 vcpkg-tool 執行檔的 SHA-256。只有會依 pinned
port hash 驗證的 source downloads 可以重用。

`verify-portable-release.ps1` 會重新計算 SHA-256、檢查 ZIP 路徑安全、版本與來源
metadata、禁止的 codec、必要檔案與完整 DLL closure。Portable stage 與下載後驗證
都會拒絕 fault-helper／test-hooks 產物。`-RunNegativeTests` 另證明錯誤 checksum、
錯誤 metadata 版本、竄改 native DLL hash 與移除 `libde265.dll` 都會被拒絕。

`BUILD_METADATA.json` schema 3 將隔離邊界寫成可驗證合約：helper role 是
`codec-helper`、Rust codec protocol 是 3、隔離格式是 `heif,tiff`、Cargo features
是 `heic,tiff`、Job Object 記憶體上限是 805306368 bytes，單次 decode deadline
是 30000 ms。build script 從 runtime 共用的 codec-protocol constants 讀取
兩項限制，並拒絕 schema 3 的固定值漂移；兩個 executable 的 protocol version
也必須和這份合約相同。

## Tag 發布與 UI gate

1. 將候選變更經 CI 合併到 `main`，再對該 commit dispatch `build-candidate`；驗證
   workflow artifact 的 ZIP、checksum、metadata、SBOM 及 feature boundary 後才可標記。
2. 在通過上述 preflight 的 `main` commit 建立並 push 精確的 `v{version}` tag。
3. 等待 `Portable Windows release` 建置完成；tag workflow 只建立 draft。
4. 從 draft Release 下載 ZIP 與 `.sha256`，先執行：

   ```powershell
   $version = (Get-Content .\src-tauri\tauri.conf.json -Raw | ConvertFrom-Json).version
   $zip = ".\ImgViewer-$version-windows-x64.zip"
   Get-FileHash -LiteralPath $zip -Algorithm SHA256
   ```

5. 解壓同一個 ZIP，在目前維護的互動式 Windows 11 實機執行；不要求 VM
   或乾淨作業系統映像：

   ```powershell
   .\scripts\smoke-native.ps1 `
     -Executable ".\ImgViewer-$version-windows-x64\ImgViewer.exe" `
     -FixtureDirectory ".\tests\fixtures"
   ```

6. 使用 GitHub CLI 驗證 ZIP 的 build provenance（將 `OWNER/REPO` 換成正式
   repository）：

   ```powershell
   $repo = "OWNER/REPO"
   $tag = "v$version"
   $commit = (git rev-parse "$tag^{commit}").Trim().ToLowerInvariant()
   gh attestation verify $zip `
     --repo $repo `
     --signer-workflow "$repo/.github/workflows/portable-release.yml" `
     --source-digest $commit `
     --source-ref "refs/tags/$tag" `
     --deny-self-hosted-runners
   ```

7. 在 workflow dispatch 選擇 `promote-draft`，輸入 tag 與剛通過 UI smoke 的
   64 字元 ZIP SHA-256。
8. Promotion job 會從 draft Release 重新下載資產，驗證 digest、來源 commit、
   metadata、attestation、錯誤來源 digest 負向案例及 DLL closure；全部通過才將
   既有 draft 公開。驗證 job 只有 `contents: read` 與 `attestations: read`，
   並執行 default branch 的 verifier；另一個 publish job 才有 `contents: write`，
   不 checkout 或執行任何 repository script。兩個 job 都要求 Release 的完整資產
   清單恰好是 ZIP、checksum、build metadata、SBOM 四個檔案，且 publish job 會
   重查四個 digest 後才執行 `gh release edit`。

如果 UI gate 失敗，禁止替換同版本資產。修正程式、提升版本並建立新 tag。

## 供應鏈邊界

- Workflow action 固定到從官方 repository tag 查得的完整 commit SHA。
- 建置前的 Cargo feature-graph gate 允許主程式使用 `image` 的 GIF/JPEG/PNG/WebP
  基礎功能，但禁止主程式啟用 `image/tiff`、拉入 `tiff` 或 libheif；helper 必須以
  `heic,tiff` 同時拉入 TIFF 與 libheif。PASS anchor 是
  `PASS codec-feature-boundary main-heif=absent main-tiff=absent helper-heif=present helper-tiff=present`。
  PE import gate 與完整 DLL closure 仍各自執行。
- SBOM 使用官方 cdxgen `v12.8.1` standalone generator/validator，workflow 內固定
  兩個 EXE 的 SHA-256，驗證後才執行；不以 lockfile 外的 `pnpm dlx` 載入工具。
  產物再加入實際封裝的 libheif、libde265 和 MSVC runtime hash；enrichment 會要求
  cdxgen 已列出 Cargo `tiff` component，並在 helper component 記錄上述六個 isolation
  properties。
- GitHub attestation 依賴 repository 與方案支援；它不是 Authenticode。
- 目前沒有設定簽章憑證、硬體金鑰或託管簽章服務，因此 ZIP、EXE 與 DLL 尚未具備
  Authenticode。加入 installer 或 updater 前必須另行完成簽章金鑰管理。
- GitHub-hosted runner 沒有互動式桌面，所以 native UI gate 明確保留為人工、
  digest 綁定的 promotion 步驟；不使用 WDIO 或 WebDriver。
