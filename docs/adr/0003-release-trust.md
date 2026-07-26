# ADR 0003：發布物必須可追溯且不可原地替換

- 狀態：Accepted
- 日期：2026-07-23

## 決策

版本、Cargo、pnpm、vcpkg 與 CI Action 都固定。正式 ZIP 只能由乾淨 tag
建置；同版本發布物不可原地替換，修正必須升版。發布資料應包含 SHA-256、
來源 commit、工具鏈與 native codec 版本、CycloneDX SBOM 和 artifact
attestation。

## 影響

互動式 Windows 測試機必須驗證 CI 產生的同一份 ZIP，不得重新建置另一份。
加入 installer 或 updater 前，ImgViewer 自有 EXE 與 helper EXE 必須先有
受妥善保管金鑰的 Authenticode 簽章。
