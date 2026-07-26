## 問題與修正

- 症狀：
- 根因：
- 修正方式：

## 驗證

- [ ] `pnpm test`
- [ ] `pnpm build`
- [ ] `cargo fmt --manifest-path .\src-tauri\Cargo.toml --all -- --check`
- [ ] `cargo clippy --locked --manifest-path .\src-tauri\Cargo.toml --all-targets --no-default-features -- -D warnings`
- [ ] `cargo test --locked --manifest-path .\src-tauri\Cargo.toml --no-default-features`
- [ ] 若涉及 UI、窗口、RAM、native codec 或發布，已執行對應 packaged smoke。
- [ ] 無法執行的驗證已在下方明確說明。

## 安全與相容性

- [ ] 未加入 PDF、網路、遙測、自動更新、shell、一般 filesystem API、
      WDIO／WebDriver 或正式版測試 plugin。
- [ ] 未提交私人圖片、完整本機路徑、secret 或未去識別化記錄。
- [ ] Capability、CSP、Tauri plugin、command、原生 DLL、輸入上限與
      `unsafe` 未變更；若有，已更新威脅模型並加入負向測試。
- [ ] 依賴更新已分成 UI、Rust 或 codec 批次，並提交必要鎖檔差異。
- [ ] 使用者可見或安全政策變更已更新 `CHANGELOG.md`。

## 補充證據或限制

<!-- 貼上精簡 PASS anchors；不要貼私人資料。 -->
