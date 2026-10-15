# Tauri async接続とIPC境界

🟢 [Done] `threex-tauri-host` はTauri 2の実invoke handler、async command、認可、3言語のエラーDTOを提供する。
Documentの所有者は従来どおりRustのJournalWorkerだけ。Tauriのmanaged stateはworkerへの接続と認可情報を持つ。
複数ウインドウは同じworkerを利用し、DocumentやGPUデータをIPCで複製しない。

## 接続

Rustホストで信頼できる保存先とWindowGrantを決め、`IpcHost::open(root, config).await` を呼ぶ。
`attach(builder, host)` はmanaged stateとinvoke handlerを登録する。Wryを有効にした実アプリの
Builderへ接続できるgeneric Runtime実装であり、このcrate自体はWryやGUIアプリを起動しない。
他のcommandと併用する場合は `invoke_handler()` を統合すること。attachは既存handlerを置き換える。
Remote ACL検査を含むこのhandlerを使い、commandだけを直接generate_handlerへ登録しない。

WebViewからTauriのinvokeを呼ぶ例：

```js
const hello = await invoke('document_ipc', { payload: { type: 'hello', locale: 'ja' } });
const request = {
  type: 'edit', document_id: hello.state.document_id,
  session_id: hello.session_id, sequence: '1',
  action: { type: 'commit', expected_revision: hello.state.revision, commands: bytes },
  locale: 'ja'
};
const result = await invoke('document_ipc', { payload: request });
```

`bytes` は既存64-byte command形式のbyte配列。応答不明時には元のrequest全体を保持して同じ内容を再送する。
再接続時にsession、sequence、expected_revisionを書き換えて再送しない。
同じsessionからの新規編集は呼出側で順序を直列化する。複数windowは別sessionとして競合revisionを検出する。
Inspect、ContentHash、Session、Saveも同じcommandから指定できる。保存先はRustで固定し、IPCにpathはない。

## 認可

- Rust設定に登録したラベルだけを許可。Editorは編集・保存・自分のsession照会、ReadOnlyは読取だけ。
- Tauriが注入したWebviewの実ラベル・所属window・実URLを検査する。本文のラベル／URLは受け付けない。
- WebviewWindowのラベル一致を要求。埋込webviewなど別形態は明示対応まで拒否する。
- アプリorigin `tauri://localhost` と `http(s)://tauri.localhost` を許可。開発originはRust設定のloopback完全一致のみ。
- Tauri ACLがremote frameを拒否する。誤ってremote capabilityを付けた場合もhandlerがRemote contextを拒否する。
  任意remote capabilityを追加しない。フロントエンドは信頼したアプリコードとCSPで運用する。
- Document IDと割当session IDを検査。これらは認証secretではなく、Rustのwindow認可を併用する。
- Rustのrevoke_windowは新規受付を停止する。受理済み処理は完了する。

## async・永続化

起動復旧、std channelの応答待ち、終了joinはspawn_blockingへ分離する。
受付は64件まで、workerの既存有界queue／group commitを維持。満杯はqueue_fullを返す。
受付番号の発行とenqueueを同じ短いlockで直列化し、await中にmutexを保持しない。
応答futureの破棄は受理済み編集を取り消さない。shutdownは新規受付を停止してworkerをdrainする。

`ipc-sessions.json` v1はDocument IDとwindowラベル別のsession IDを保存する。
workerのwriter lock取得後、初回hello前にtemp書込・fsync・rename・directory fsyncで公開する。
最大64ラベル／64KiB、文書不一致・破損・重複sessionは起動失敗とし、黙ってIDを交換しない。
このファイルは文書checkpoint／journalと一緒に保持する。旧manifestがない場合だけ初期割当を行う。
旧sessionを含む再送を新sessionへ自動変換しない。window閉鎖はEndSessionではない。
明示終了後も同じbindingを維持し、過去の再送は既存receipt／watermarkで重複実行を防ぐ。
新sessionへの明示的な再割当UIは未実装。

編集はすべて永続ProtocolRequestを経由する。IPC transport番号はRustで採番し再起動でリセットしてよい。
永続的な重複排除はDocument ID／session ID／sequence／元payloadが担う。
受理済み失敗の結果もv4 journalで保存され、再起動後に当時のエラー・revisionを返す。
ディスク形式は既存v4のまま。

## DTO

version 1のrequestはtype付きenumで未知fieldを拒否する。command byte列は256KiBまで。
この上限はDTO検証の上限であり、TauriがJSONを解析する前の通信サイズ制限ではない。
u64のrevision、sequence、generation、統計値は正規化10進文字列で渡し、JavaScriptの整数丸めを避ける。
負数・指数・小数・先頭ゼロ・範囲外・JSON数値のsequenceは受付前に拒否する。
stateはID、revision、object数、dirty等の小さい状態だけを返す。

ErrorDtoは `code`, `message`, `observed_revision`, `retry`。
英語・日本語・简体中文でmessageを翻訳し、codeと数値契約は共通。
OSの詳細エラーや保存先は公開しない。永続失敗のobserved_revisionは失敗当時の値で、現在値はInspectで読む。
retryはsame_request／new_request／resynchronize／none。受理済み編集失敗はsequenceを消費するため、
修正した新しい編集は次のsequenceを使う。通信／IOの結果不明は元requestの再送で確認する。

## 検証と制約

🟢 [Done] Tauri MockRuntimeの実invoke経路を使う9テスト：複数window共有・保存・再起動再送、
永続エラーの同一DTO、認可／origin／別session／revoke、strict DTOとu64精度、有界待機、
応答破棄とdrain、明示session終了、破損manifest、全worker codeの3言語翻訳。
既存storageの切断位置・破損末尾・process exitテストも回帰実行する。

⭕️ [Pending] 実Wry WebView／OS別capability・CSP運用、native surfaceの実OS表示、
ドッキング／フローティングUI、実電源断。MockRuntimeは実ウインドウやGPUの検証ではない。

English: Tauri async IPC shares one Rust-owned Document worker, authorizes the actual caller,
and preserves durable request identity across restart. Errors have stable codes and localized messages.
简体中文：Tauri异步IPC共享Rust持有的Document worker，验证实际调用窗口，重启后保留请求去重。
错误代码稳定，消息支持英语、日语和简体中文。实际WebView与GPU验证仍待完成。

🟢 [Done] Rust Renderer／native surface接続・世代付きlifecycleとworker Scene借用：[仕様・検証](native-renderer.md)。実OS／GPU表示は⭕️ [Pending]。

🟢 [Done] native Windowイベント自動配線とon-demand redraw scheduler：[仕様・検証](native-redraw.md)。実OS／GPUイベント・表示は⭕️ [Pending]。
