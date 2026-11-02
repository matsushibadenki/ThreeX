# G03-B2b1：多段LOD assetの保存・有界読み込み

🟢 [Done] `THREEXA2`の多段階層metadata、生成親geometryのpage化、immutable保存とpage単位の検証・読み込み。

🟢 [Done] G03-B2b2：[有界worker配送と階層cut／GPUコード接続](native-hierarchy-delivery.md)。現行workerとRendererはv1／v2を扱う。実GPU検証はPending。

🟢 [Done] G03-B2b3：[background builder／生成結果cache](background-hierarchy-builder.md)。

🟠 [Next] view全体の訪問budget・詳細LOD履歴／cut cache。G03-B全体の完了ではない。

## Rust API

`HierarchyManifest::prepare(id, revision, &SimplifiedClusters)`は親生成結果からmetadataとAssetRefを生成する。入力は検証済みbuilder出力を変更せず渡す。`encode_page`は元vertex ID順に参照頂点だけをコンパクト化し、座標・triangleの向き・別IDの同一点を維持する。返り値のerror評価を再計算するAPIではなく、bounds・サイズ・indexと宣言errorの階層関係を検証する。

`threex_storage::hierarchy_asset::write_hierarchy_asset`は出力先、ID、revision、生成結果、file容量上限を受け取る。同一ディレクトリのcreate-new一時ファイルへmetadataとpageを順次書き、fileをfsyncした後、hard linkで既存宛先を置き換えず公開し、親directoryをfsyncする。公開後のエラーでは完成した宛先が残る場合がある。エラーを「公開されなかった」と同一視しない。通常エラー時の一時ファイルはbest-effortで削除し、プロセス中断時の残存一時ファイルは自動採用しない。既存宛先は成功・失敗を問わず上書きしない。

`HierarchyAssetReader::open(path, expected_ref)`はheaderと固定上限のmetadataだけを読み、ファイル長も検証する。`read_page(expected_ref, node_id)`は参照一致とnode IDを確認してから対象pageだけをseek／readし、hash・bounds・indexを検証して`HierarchyPage`を返す。返り値はRust所有のbytesであり、WebViewへ転送しない。Document／surface job guard、residencyの予約とstale結果拒否は呼び出し元の責務で、この専用readerとは別に、既存の統合readerを通す[worker接続](native-hierarchy-delivery.md)を実装した。

## Format

すべてlittle endian。metadataのSHA-256を既存AssetRefのhashとして使い、各pageのSHA-256をmetadataに含める。Document／checkpoint／journalの形式は変更しない。

| 項目 | 形式 |
| --- | --- |
| header | 72 bytes：magic `THREEXA2` (8)、asset ID (16)、revision u64、root bounds f32×6、file長 u64、node数 u32、reservedゼロ4 bytes |
| node record | 88 bytes：offset u64、length u64、vertex数 u32、index数 u32、error f32、SHA-256 (32)、node bounds f32×6、parent ID u32 |
| root parent | `u32::MAX` |
| node／page ID | record index。root=0、親は常に子より小さいID。子の列挙順はID昇順 |
| payload | compact vertex f32×3の配列、続いてlocal triangle index u32の配列 |

headerのboundsとroot recordのboundsが一致すること、親boundsが子を包含すること、errorが有限・非負で親以上へ増加しないことを検証する。親IDを小さい値に限定して、循環・孤立nodeを拒否する。pageはmetadataの直後からrecord順に隙間なく配置し、終端と宣言file長を一致させる。予約値、長さ、overflow、不正vertex／index、checksum不一致を拒否する。

## 容量とコスト

nodeは最大4096、metadataは最大360,520 bytes（72 + 88 × 4096）、pageは既存と同じ最大1 MiB。最大サイズの全pageを置くとpayloadは4 GiBになるため、保存APIの`max_file_bytes`で用途に応じた合計上限を別途指定する。この固定metadata版は、数千億triangleデータセット全体を単一manifestへ収める最終形式ではない。metadata自体のpage化は今後の拡張。

保存はmetadata準備で各pageを一度encodeしてhashを求め、書き込み時に再encode・検証する二段階。asset全体のpayloadをまとめて保持せず、sourceのほかに固定上限のmetadataと1 page分のpayload・compact ID対応表を保持する。encode用の対応表はpage index数／vertex数で有界だが、allocator overheadを含む厳密なbytes quotaではない。読み込みはmetadataと要求された1 page分に限定する。同時実行は既存loaderの2 thread／8 leaseで制限する。

## 互換性・検証

既存の`AssetManifest`／`AssetReader`は`THREEXA1`と`THREEXA2`を判別し、v2を2ページへ変換せず全階層を保持する。v1／v2のpayload検証を共有して同じ1 MiB・bounds・index規則を使う。既存のPagePool、root／proxy fallback、Instance差分転送、gather cache、再送重複排除には変更を加えていない。

テストでは3段7nodeの保存・再open、geometry位置とwindingの一致、決定性、容量超過、古い参照、不正node、再hashした不正metadata、破損page、切断末尾、一時ファイル中断と宛先上書き拒否を確認する。破損した子を除外するとCPU coverage選択がrootを保持し、rootの検証済みpageを読み続けられる。v2の実GPU fallbackを検証したという意味ではない。

検証結果：Rust workspaceはdebug／releaseとも209 passed（core 74、renderer 43、storage 74、Tauri host 18）。JS 13 passed、Clippy all-targets／all-features、format、diff checkも通過。

⭕️ [Pending] v2の実GPU描画・性能、実電源断、macOS以外のhard link／directory fsync durability。現環境で検証できないものはCPU／filesystemテストと区別する。

Universal-3DとのRust Document所有・world bounds・差分更新の契約を維持する。MuTracerのコード・shader・テスト・生成物は流用せず、両参照ディレクトリへの依存を追加しない。

English: THREEXA2 persists parent-first multilevel LOD metadata and independently checksummed compact pages. Publication never overwrites an existing asset; readers bound metadata and read one requested page at a time. THREEXA1 and the current renderer path remain unchanged. Worker/GPU code is connected; background building remains Next and GPU output remains unverified; cross-platform durability and GPU validation are Pending.

简体中文：THREEXA2保存父节点优先的多层LOD metadata和独立校验的compact page。发布不覆盖已有asset；读取限制metadata大小且仅读取请求的page。保留THREEXA1和现有绘制路径。已连接worker／GPU代码，后台构建仍为Next，实机GPU验证仍为Pending，跨平台持久性与实机GPU验证保持Pending。
