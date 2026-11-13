# Projectの永続化、正確な時刻、復旧

Web serverはRUNS_DIR/.projectsを開き、ProjectAPI経由で/api/v1/projectsを提供します。Scenario版とRun入力は不変です。条件編集では新しいScenario版を保存してからRunを作ります。Branch位置は、integrity確認済みCheckpointの保存後にだけ進めます。browser再読込後も同じIDを参照できます。server再起動時は中断済み作業として記録し、最後に完了したCheckpointから再開します。不完全なchunkを完了扱いしません。

同じ操作をCLIから利用できます。

~~~text
sheltersim project --root runs/.projects create --project-id study
sheltersim project --root runs/.projects save-scenario --project-id study --scenario scenario.json --approved-hash <reviewed-canonical-sha256>
sheltersim project --root runs/.projects new-run --project-id study --run-id trial --scenario-id <id> --scenario-version 1
sheltersim project --root runs/.projects resume --project-id study --run-id trial --minute 100
sheltersim project --root runs/.projects frame --project-id study --run-id trial --minute 73
sheltersim project --root runs/.projects fork --project-id study --run-id trial --minute 73 --child-id alternative
~~~

approved hashは確認した正準ScenarioSpecそのものに結び付きます。Webの計算開始（POST R/resume）は、BranchのポインタとScenarioの期間だけで目標時刻を検査して受け付けます（head Checkpointに記録したモデルの版も読み、旧モデルのRunはここで止めます。下の「旧モデルのRun」）。head Checkpointの展開と検証はworkerが1回だけ行い、破損や非互換を検出した場合はRunを `failed` にして、Branchのポインタは変えません。CLIとWebの継続実行は設定されたLLM modeを使い、モデル設定と費用記録を保持できます。過去frameの読み出しではnetwork adapterを作りません。offline履歴が暗黙にlive履歴へ変わることはありません。記録が欠落または非互換なら、過去の再構築を拒否します。

時刻Mはtick Mの直前境界です。timelineのWorldState KPIはその境界で取得します。保存metrics.csvはtick終了時刻の意味を保ちます。frameにはexact、state_hash、computed_untilを含み、未計算時刻は拒否します。親と兄弟のstateは独立しています。geometry点を物理メートルとして扱うのは検証済みmovement位置がある場合だけです。旧来の表示用位置はdisplay_onlyと表示します。

Checkpointは `runs/{run}/checkpoints/` に、目録（manifest、`{checkpoint_hash}.json`）とgzip圧縮したchunkで保存します。chunkは正準JSONのSHA-256を名前に持ち、同じ内容は一度だけ書きます。目録の `checkpoint_archive_v3` では、1回の保存で新しく増えたchunkを1つのpackファイル（`checkpoints/packs/{packのSHA-256}.pack`）にまとめて書き、目録の `locations` に、各chunkがどのpackの何byte目から何byteかを記録します。前の保存のpackにあるchunkは書き直さず、その場所を参照します。目録のhash（`manifest_hash`）は `locations` も含み、読むときは今までどおりchunkごとに名前のSHA-256と照合します。これより前の `checkpoint_archive_v1`・`v2` の目録は、chunkを1つずつのファイル（`checkpoints/chunks/{SHA-256}.json.gz`）に置いていて、そのまま読めます。v2のRunを続けて計算すると、既存のchunkファイルを参照し、増えた分だけをpackに書きます。Checkpointの内容とhashは、置き方によらず同じです。v3の目録は、v3に対応する前の版のコードでは「unsupported checkpoint archive version」として読めません。保存1回で書くファイルとfsyncはpack1つ分になり、容量の計測で数えるファイルも、保存の回数程度に収まります。目録の `checkpoint_archive_v4` は、前の保存の目録（base）と違う所だけを持ちます（#138）。baseは、そのBranchの前の保存のCheckpoint、新しい子Branchの最初の保存では分岐元のCheckpointで、目録の `base` にそのhashを、`depth` にbaseをたどる段数を書きます。`document` では、baseと同じ節点を `{"kind": "base"}` に、先頭のchunkがbaseと同じ索引つきの一覧を `list_v2_tail`（baseの先頭のchunkの数 `base_chunks` と、その後ろのchunkと開始位置）にします。`locations` は、baseの目録とそのbaseが持たない場所だけです。読むときは、baseを順にたどって全体の文書と場所に戻し、それから今までどおり読みます。各目録の `manifest_hash`（`base` と `depth` を含む）、baseのhashと段数を確かめ、合わなければ読みません。たどる段数は31までで、32段目になる保存は文書の全体を書きます（`depth` 0）。100人・7日分のRunでは、目録が1つ0.26〜0.48 MB・合計87 MBから、この形で小さくなります。v3の目録は今までどおり読め、v3の目録をbaseにもできます（`depth` 1）。同じCheckpointを別のBranchが保存済みのときは、その目録をそのまま使います。目録は個別に消さないので（Runの削除だけがまとめて消します）、baseが先に消えることはありません。v4の目録は、v4に対応する前の版のコードでは「unsupported checkpoint archive version」として読めません。128項目を超える一覧は、索引つきの一覧（`list_v2`）としてchunkに分けます。追記だけの一覧（`event_log`、`metrics_state`の`snapshots`・`resource_snapshots`）は、Checkpoint v2のhashと同じ128項目ごとに分けます。ほかの一覧（`model_state`の最大2万件の履歴、`support_requests`など）は、項目の正準JSONから作る64 bitの値（`native/src/checkpoint.rs`の`content_chunk_starts`）が128で割って127余る項目の後で区切り、1つのchunkは512項目までにします（#138）。区切りが項目の中身だけで決まるので、古い項目を先頭から捨てても、途中に項目が入っても、変わるのはその前後のchunkだけです。128項目ごとに区切ると、先頭の項目が1つ減るだけで後ろのchunkがすべて変わり、保存のたびに書き直していました。区切り方は保存の置き方だけで、Checkpointのhash（128項目ごと）と内容は変わりません。目録の`starts`は前から各chunkの開始位置を持つので、読み方も変わりません。

Checkpoint v2のhash（`state_hash`・`checkpoint_hash`）は、文書全体のSHA-256ではなく、chunkのhashの木の根です。`event_log`、`model_state`の追記専用の履歴（`body_state_v1.history`、`shared_operations_v1.history`、`resource_ledger_v1.audit`、`resource_service_v1.request_history`・`queue_stall_history`）、`metrics_state`の`snapshots`・`resource_snapshots`は、128項目ごとの正準JSONのSHA-256（archiveのchunkの名前と同じ）と項目数からhashを作ります。そのほかの値は正準JSONのSHA-256で、これらを含むobjectはフィールドごとのhashからhashを作ります。保存は新しいchunkだけをhashします。決まりは文書だけで決まるので、archiveのどの版からでも、単独のJSONからでも同じ値になります。`schema_version`が1のCheckpointは、従来どおり文書全体のSHA-256で確かめて読み、再生・分岐・比較に使えます。v1の先頭から計算を続けると、それ以降の保存はv2になります（v1の版のコードではv2を読めません）。計算の結果を比べるhash（`benchmarks/investigate_performance.py`のworld・events・KPI、CLIの`final_state_hash`）は正準JSON全体のSHA-256のままです。Inspectorの`state_hash`の表示は、Checkpointの持つ値なので、v1の時刻とv2の時刻で決まりが異なります。分岐のプレビューが持つhash（`WorldState.state_digest`）は今の版の決まりで計算し、v1のCheckpointから分岐するときは、そのCheckpointの`state_hash`ではなく復元した世界の`state_digest`と比べます。

Checkpointを保存したworkerは、その時点のframeをメモリ上のWorldStateから描画し、`runs/{run}/frames/{checkpoint_hash}.json.gz` に保存します。frameはCheckpointから導出できる置き換え可能なread modelで、`schema_version: 1`、描画版 `timeline-frame-v1`、元の `checkpoint_hash`、frame本体のSHA-256を持ちます。timeline GETは、要求時刻が保存境界で、キャッシュの版・hash・Run・Branch・時刻が一致する場合だけそのframeを返し、`computed_until` を現在のBranch値に置き換えます。それ以外の時刻、旧Run、欠落・破損・版不一致のキャッシュは従来どおりCheckpointから復元して描画します。GETはキャッシュを書き込みません。キャッシュの書込失敗や容量超過はCheckpoint保存を失敗させず、次の読取が復元経路になるだけです。描画結果を変える変更では `FRAME_PROJECTION_VERSION` を上げます。

Runの計算の状態（queued、running、paused、completed、failed、interrupted）は、Branchごとに `runs/{run}/branches/{branch}/status.json`（`schema_version: 1`）に記録します（#48）。Runの `status.json` の `status` は、どれかのBranchが計算中ならrunning、そうでなければ最後に止まったBranchの状態（分岐の後は分岐の前の状態）を要約として持ちます。Runの参照（`GET …/runs/{run}`）の `branches` の各要素には、記録があればそのBranchの `status` が付きます。この記録より前に計算したBranchには `status` がありません。分岐の途中でプロセスが終わり、子Branchが作られなかった場合は、子の状態の記録を消します。サーバーの起動時の回復は、計算中のまま書込権を持つプロセスがいないBranchを `interrupted` にします。archiveは記録を保持するだけで、disk容量を空けません。deleteは非稼働Runに対し、同じRun IDを明示確認した場合だけ実行でき、そのRunだけを削除します。ProjectStoreは容量とfile数を返します。容量は呼ばれるたびにdisk上の通常fileを数え直すので、ほかのprocessが書いたfileも含みます。ただしCheckpointの保存1回の中では、最初に1回だけ数え、それ以降はその値に、その保存が書いたbyte数とfile数を足して使います。保存の途中でほかのprocessが書いたfileは、次に数え直すときから数えます。そのprocessは書く前に自分で数え直すので、容量上限を超えるのは、その保存が書く分までです（#237）。symlinkは数えず、たどりません。数えている間に消えたfile（書込途中の一時fileなど）は0 bytesとします。容量上限を超える書込は、保存fileでも履歴索引でも拒否し、書く前の状態に戻します（#47）。Checkpointは版管理されたgzip chunkとcontent hashを使い、公開Checkpoint JSON v1と従来のraw保存も読み込みます。Scenario、Run、Branch IDは実在避難者名ではなく安定識別子です。credential fieldを拒否し、一般的なsecret/contact文字列をredactまたは拒否します。メールアドレスは、`@`のあとのドメインの末尾が英字などの文字のものだけを検出します（`storage/projects.py`の`EMAIL_ADDRESS`。Run成果物と保存Runの表示も同じ定義を使う）。モデルの署名のように`@`のあとに小数点を含む値は、連絡先とみなしません（#187）。電話番号は、区切りのある3つの数字の組（`090-1234-5678`、`+81 90 1234 5678`など）を検出します（`PHONE_NUMBER`。Run成果物と保存Runの表示も同じ定義を使う）。論文のDOIや日付は電話番号とみなしません。区切りのない数字の並びは検出しません（#190）。Checkpoint保存時のこの検査は、archiveがまだ保存していないchunkの値と、分割されたobjectのフィールド名に対して行います。既存chunkは最初の書込時に同じ検査を通っています。新しいchunkは全検査が通ってから書き込むため、拒否されたCheckpointは何も書きません。workerが `capture()` した直後のCheckpointは `save_captured_branch` で保存し、`capture()` 時の検証を再実行しません。このときarchiveは、`capture()` が再利用した履歴の正準JSON byte列からchunkを作り、分割の判断に使う大きさも同じbyte列から求めます（#44）。それ以外の入力は `save_branch` が再検証します。保存済みCheckpointの読込は、展開した文書をコピーせずに検証し、その検証で復元した世界と指標を、計算の続き（`advance`）や途中時刻の再計算（`seek`）でそのまま使います（`load_restored_checkpoint`、#46）。ProjectStoreは、最後に読み込んだ2つのCheckpointについて、復元した世界と指標の手つかずの複製をcheckpoint hashごとに持ちます（比較と再生は2本のBranchを読むため）。同じCheckpointをもう一度読むときも、文書の展開とmanifest・chunk・`checkpoint_hash`・`state_hash`の確認は毎回行い、復元の代わりに持っている複製のコピーを返します。Checkpointの文書は内容のhashで名前が決まり変わらないので、同じhashの文書から復元した状態は同じです。コピーでは、変更されないEventLogの記録と予定イベントを共有します。持ち続ける世界の分だけ、Webサーバーのプロセスのメモリが増えます（#245）。複製はEventLogの記録を元の世界と共有するので、持っている間はその記録も残ります（1440分の世界1つで、Pythonのメモリ約140 MiB）。読み終わったあとまでメモリを占めないよう、どの複製も最後の読込から60秒使われなければ捨て、循環参照の回収を1回行います。計算を進めるときの読込（`advance`）では複製を作りません。先頭のCheckpointは計算が保存するたびに新しくなり、読み直されることがほとんどないためです。

保存の間の時刻の読取り（timeline GETで保存境界以外の時刻、replay-frames）は、直前のCheckpointを復元して1分ずつ計算し直します。新しい画面の再生は8分ずつの窓を続けて求めるので、窓ごとに同じCheckpointを復元し直していました。ProjectStoreは、読取りが計算し終えた世界（`storage/read_worlds.py`の`ReadWorldCache`）を、Project・Run・Branchと、読んだときのBranchの先頭のCheckpointのhashとともに持ちます。次の読取りは、同じ先頭の下で、求めた最初の時刻以前で、その時刻の前の保存の時刻以後（または8分前以後、`READ_TICKS_BEFORE_SAVE`）の世界があれば、その世界から計算を続けます。なければ従来どおり復元します。続きを計算する世界は、1回の長い`iter_worlds`と同じく、Checkpointから復元した世界に1つのcontrollerとengine、記録した応答だけを返すrouter、Checkpointの記録を種にしたhistory cacheを持ちます。frameは、持っていない場合と同じbyte列になります（7日分の見本のRunの両Branchで、Rustのkernelあり・なし（`SHELTERSIM_NATIVE=0`）とも、連続した窓と前後の時刻を比べて確かめました）。

- **持ち主：** 読取りは世界を取り出して（`take`）持ち主になり、読み終えたら戻します（`put`）。同時に2つの読取りが同じ世界を持つことはなく、もう一方は復元します。途中で止まった読取り（例外、generatorを閉じた場合）は世界を戻しません。世界は保存済みCheckpointから読取りのために復元したもので、計算中のBranchの世界とは共有しません。読取りは世界を変えず（描画と検証だけ）、Branchの保存、乱数、Providerに触れません。分岐（`fork`）、介入の確認（`seek`）、計算の続き（`advance`）、PDFレポート（`view_at`）は使いません。
- **上限：** 1つのBranchにつき2つ、プロセス全体で4つ（`SHELTERSIM_READ_WORLDS`で全体の数を変えられ、0で使いません）。古く使ったものから捨てます。どれも最後の使用から60秒で捨て、循環参照の回収を1回行います。7日分の100人の世界は1つで数百MiBになります。
- **捨てるとき：** Branchの先頭が変わると（計算の続き、別のプロセスの保存）、次にそのBranchを読むときに古い先頭の世界を捨てます。このProjectStoreがBranchを保存するとき（計算、分岐）、新しいBranchの登録を取り消すとき、Runを削除するときにも捨てます。旧モデルのRun（閲覧のみ）は保存の時点を読むだけなので、持ちません。
- 各分の`world.validate()`は従来どおり行います。会話記録はserverが検証したRun/Branch/時刻に結び付くEvidenceReferenceを使い、browserから送られたkeyを保存しません。

Runの履歴（`history`）は、計算しながら書く履歴索引（`runs/{run}/history.sqlite`）から読みます。索引には、Checkpointから作れる記録（イベント、KPI、社会・職員・人口の履歴など）のほかに、計算中に記録した行動・会話の判断（候補、方針の版など）が入ります。判断はCheckpointに入らないので、索引を作り直すと戻りません。そのため「既存Runの履歴索引を再構築」（`POST R/index`）は、索引が分岐の先頭のCheckpointと一致しているとき（状態`ready`）は`history_index_current`で断ります。作り直せるのは、索引のない古いRun、先頭がずれた索引、版5までの索引（下）だけです。作り直した索引には判断の記録がなく、応答の`limits`にそのことを書きます（#213）。索引への登録はCheckpointの保存1回ごとに1つのトランザクションで行い、記録を数百件ずつまとめて書き込みます。索引はSQLiteの従来のジャーナル方式で書きます。WAL方式（#268）は、すべてのページをログと本体に2回書くため、1日分の登録のcommitが約2倍かかり、WSLでは時々10〜15秒長くなったので戻しました（#269）。WAL方式のときに書いた索引は、次の書込みで従来の方式に戻ります（ほかの接続が開いている間は、その次の書込みで戻ります）。すでにある記録と同じkeyの記録は追加せず、内容が違えば`immutable_history_record_conflict`で拒否して、その登録全体を戻します（#47）。保存はBranchのポインタを新しいheadに書き換えてから索引に登録するので、その間の読取りでは、索引がポインタより後れています。保存が8回以上ある長い計算では、索引への登録を別のプロセスで計算と並行して行います（`storage/index_writer.py`、`BackgroundIndexer`）。計算側は保存のたびに、前の保存から増えたEventLogの記録とKPI・資源の標本、登録が読む時刻以後の履歴の項目、model_stateの名前と設定だけを送り、登録のプロセスが保存の順に1つずつ登録します。送った分の先頭が索引の記録している位置と合わなければ、`history_index_out_of_step`で断り、計算を失敗にします。保存1回分の送信はパイプの大きさを超えるので、登録のプロセスは別のスレッドでパイプを読み、1つの接続とそのページキャッシュを保ち続けます（ほかの書き手が書くと、SQLiteがキャッシュを捨てます）。登録が2回分より遅れると計算側が待ち、計算の終わりには登録が追いつくのを待ってから状態を`paused`・`completed`にします。そのため計算中の索引は、ポインタより最大で保存2回分後れます。計算のプロセスが途中で止まると、まだ登録していない保存のCheckpointは索引の一覧（`checkpoints`）に載りません。次の計算は索引の記録している位置から登録を続けるので、記録は欠けませんが、その時刻の過去の参照は、一覧にある前のCheckpointから計算し直します。この間も、frame・比較・再生は索引のheadまでのCheckpointと新しいheadで答え（#259）、履歴（`history`）・Checkpointの一覧・会話記録は索引のheadの時点で答えます（#262）。履歴とCheckpointの一覧の応答の`anchor_hash`と`indexed_through_minute`は、答えに使った索引のheadとその時刻です。履歴の検索は1つの読取りの中で索引のheadの時点に揃うので、検索の途中でポインタが動いても`history_changed_during_query`で断りません。ページ送りのcursorはそのheadに結び付き、headが変わると従来どおり`stale_or_foreign_cursor`になります。索引がポインタより先にある場合や、Branchの行がない場合は、従来どおり`history_index_stale`です。分岐した子Branchは、分岐元の行を写さず、分岐の時刻より前の行は分岐元の索引から読みます（#256）。索引の`lineage`表に、子Branchごとに分岐元と分岐の時刻を記録し、履歴の検索は「その Branch の行」と「分岐元の、分岐の時刻より前の行」（分岐元がさらに分岐していれば、その分岐元の、小さい方の分岐の時刻より前の行）をあわせて読みます。会話記録は Branch ごとのもので、分岐元のものは読みません。子Branchが登録するのは分岐の時刻以後の行だけで、分岐元が分岐の時刻より前に持っている行と同じkeyの行は登録しません（内容が違えば`immutable_history_record_conflict`）。そのため分岐は、索引の行を写す時間も、そのぶんの容量も使いません。子Branchは、分岐元が計算中に記録した、分岐の時刻より前の行動・会話の判断も読めます（Checkpointから作れないため、以前は子Branchにありませんでした）。作り直し（`POST R/index`）をしたBranchは、すべての行を自分で作り、分岐元を読まなくなります。`lineage`表は索引の版2で加えました。索引の版3では、記録の内容（`records`の`payload`、正準JSON）を圧縮し、BLOBとして持ちます（#80、`storage/history_payloads.py`）。先頭の1 byteが形式で、1は「辞書1を前置したraw deflate（RFC 1951、窓2^15、zlibの見出しと検査値なし）」です。続くbyte列をその辞書で展開すると、UTF-8の正準JSONになります。辞書1（`src/sheltersim/storage/history_payloads_1.zdict`、32 KiB以下）は、生成Scenario（合成の自治体、地震、100人、seed 2026）を1日分計算した索引の行だけから`scripts/build_history_dictionary.py`で作った固定のファイルです。実在の人の記録からは作りません。コードにSHA-256を書いておき、読み込むときに照合します（違えば`history_index_dictionary_mismatch`）。この辞書で圧縮した行はbyteまで同じ辞書でないと読めないので、一度使った辞書は変えも作り直しもしません。別の辞書を使うときは新しい形式の番号に割り当て、前の辞書も残します。圧縮はzlibの水準3で、索引に書くプロセス（長い計算では登録のプロセス）が行います。長い計算では登録のプロセスの速さが計算全体の時間を決めるので、圧縮の時間はそのまま計算の時間に足されます。水準6は、縮む量が1割ほど増えるだけで、圧縮の時間が約1.5倍かかります。`payload_hash`は今までどおり正準JSONのSHA-256です。圧縮したbyte列はzlibの実装や版で変わりうるので、比べるとき（同じkeyの記録の不変性の確かめ、分岐元の行との照合）も、hashを確かめるときも、読むとき（検索、一覧、比較、会話、因果、実験の結果の収集）も、展開した文字列を使います。壊れた圧縮の行は`history_index_record_corrupt`、知らない形式の行は`history_index_payload_format_unsupported`で断ります。Checkpoint、frame、計算の結果は変わりません。

索引の版4（`PRAGMA user_version=4`、#130）は、キーと、何度も現れる文字列を1か所だけに持ちます。版3までは、記録のkey（平均約50文字。イベントは64文字の16進のhash）を、`records`の主キーと副索引6つ、`entities`（記録1件あたり約5行）、`tokens`（約4行）に文字列のまま持ち、ファイルの約9割がキーと索引でした。版4では、`records`の各行に整数の`id`を付け、keyは`records`とその一意な索引`record_key`（Branch・種類・key）だけに持ちます。`entities`・`tokens`・`links`の行は、名前の番号と記録の`id`の組だけです（主キーは名前・記録の順で、rowidなしの表）。イベントへの参照は、版3では`tokens`に`@event:`を付けた語として入れていましたが、版4では`links`表に分けました。Branch、イベントとその原因のID、`source`、種類（`event_type`など）、`status`、entity、語は、`names`表に1回だけ持ち、各行はその番号を持ちます。記録の種類（`kind`）は、名前の順に番号を決めた固定の一覧（`history_index.KINDS`、`kinds`表にも同じものを書きます）の番号です。番号の順が名前の順と同じなので、検索の並び（時刻・種類・key）とcursorは版3と同じです。種類を増やすときは版を上げます。`payload_hash`は、SHA-256の32 byteをそのまま持ちます（版3は64文字の16進）。副索引は、時刻（`record_time`）、`event_type`・`source`・`status`での絞り込み、因果の`event`・`cause`（イベントの行だけの部分索引）で、どれも整数の列だけで、keyを持ちません。同じ時刻の行は、検索のときにkeyで並べ替えます。新しい名前の番号は、書く順に、記録ごとに名前を並べてから振るので、同じ記録を同じ順に書けば、どのプロセスでも同じ番号になります。索引を書き続けるプロセス（長い計算の登録のプロセス）は、確定した書込みで読んだ名前の番号を20万件まで覚えて使い、取り消された書込みで足した名前は覚えません。100人・7日分の生成Scenarioでは、`history.sqlite`が約1.81 GBから約0.60 GBに、Run全体が約2.23 GBから約1.03 GBになり、計算は約159〜177秒から約80〜84秒になりました（登録のプロセスの書込みが減り、計算側のfsyncも短くなるため）。同じRunの検索・因果・会話・Checkpointの一覧の答えは、版3と1 byteも違いません。ただし因果のたどりが500件で打ち切られるときは、どのイベントを残すかを、つながりの時刻とkeyの順で決めるようにしました。版3ではSQLiteが選んだ索引の順で決まっていたので、打ち切られたグラフの中身は版3と違うことがあります。番号は書いた順に振るので、並行して書くBranchがあると番号の順が変わりますが、答えは変わりません。数字と測り方は#130に残します。

索引の版5（`PRAGMA user_version=5`、#138）は、EventLogの記録1件だけから作る行の内容（`payload`）を持たず、その記録のEventLogの中の位置（`records`の`position`）を持ちます。該当するのは、イベントの行（`events`）、そこから作る`social`の行（keyが`event:`）と、行動・ケア・運営の行（keyが`applied:`）です。読むとき（検索、因果）は、行のBranchの索引のheadのCheckpointから、その位置の記録をchunkごとに1回だけ読み（`ProjectStore.checkpoint_items`、`CheckpointArchive.list_items`）、索引に書いたときと同じ関数で行を作り直し、公開の検査（`safe_row`）を通してから、`payload_hash`（正準JSONのSHA-256）と照らします。EventLogは追記だけなので、headのCheckpointは、そのBranchが登録したすべての記録を同じ位置に持ちます。分岐した子Branchが読む分岐元の行は、分岐元の索引のheadから読みます。作り直した行が`payload_hash`と違うとき（行の作り方をコードで変えたときなど）は、古い答えを作り直した値で読み替えず、`history_index_record_corrupt`で断ります。`dialogue`の行は、体の状態の履歴（最大2万件で古い分がCheckpointから落ちる）を使うので内容を持ち続けます。KPIの標本、model_stateの履歴の行、計算中に記録した判断、会話記録も今までどおり内容を持ちます。同じkeyの記録の不変性の確かめと、分岐元の行との照合は、内容の代わりに`payload_hash`で比べます。検索の答えは版4と1 byteも違いません（`tests/integration/test_decisions_row.py`の、すべての行の内容のdigestは版4のときと同じです）。数字と測り方は#138に残します。

索引の版6（`PRAGMA user_version=6`、#138）は、索引を3つの点で小さくします。1つめに、`entities`には、人・世帯・職員・区域・会話の絞り込み（`agent_id`、`household_id`、`staff_id`、`zone_id`、`conversation_id`）が当たりうる名前だけを入れます。名前が`_id`・`_ids`で終わる項目のうち、イベント・行動・出典・モデル・話題・作業・依頼・設備・資源などのIDだけを持つ項目（`history_index._NOT_ENTITY_KEYS`。Rustの`native/src/checkpoint.rs`にも同じ一覧があります）の値は入れません。それ以外の項目の値は、今までどおり入れます。イベントのIDで絞る検索と因果のたどりは、`links`と`event`・`cause`の列を使うので変わりません。2つめに、64文字の小文字の16進のkey（イベントや行の内容のSHA-256）は、その32 byteで持ちます。読むときと並べるときは、16進の文字列に戻した値（`lower(hex(key))`）を使うので、同じ種類の中にhashのkeyと`applied:`などの文字列のkeyがあっても、並びとcursorは版5と同じです。3つめに、イベントから作る`dialogue`の行（keyが`event:`）も、記録の位置を持ち、内容としては体の状態の効果（`observed_model_effects`）だけを持ちます。読むときは、イベントから作った`social`の行とその効果から、索引に書いたときと同じ関数で行を作り直し、`payload_hash`と照らします。効果は、索引のheadのCheckpointの履歴から落ちていることがあるので、索引に残します。検索・因果・会話の答えは、人・世帯・職員・区域の絞り込みにそれらのIDを渡すかぎり、版5と変わりません。数字と測り方は#138に残します。

版5までの索引は、移さずに断ります（版4と版5も#130と同じ扱いにしました。#138）。読む（検索、会話、実験の結果の収集）ときも、書く（計算の続き、分岐、会話の追加）ときも`history_index_rebuild_required`で断り、計算の続きと分岐は、計算を始める前に断ります。索引の状態（`GET R/index`）は`rebuild_required`になります。どの版でも形が同じ`heads`と`checkpoints`の表だけは読むので、保存した時刻の表示（frame）、途中の時刻からの計算し直し、Checkpointの一覧は、作り直す前でも使えます。「既存Runの履歴索引を再構築」（`POST R/index`）は、版5までの索引のとき、どのBranchを指定しても、RunのすべてのBranchをそれぞれの先頭のCheckpointから一時ファイル（`history.sqlite.rebuild`）に作り直し、古いファイルの`checkpoints`表の行（Branchのポインタの範囲内のもの）と会話記録を加えてから、古いファイルと置き換えます。応答の`rebuilt_branch_ids`に作り直したBranchを並べます。Windowsは、ほかの接続が開いているファイルを置き換えられないので、短い読取り（状態の問い合わせ、frame）が終わるのを最大5秒待ち、それでも置き換えられなければ`history_index_unavailable`で断ります。途中で失敗したときは一時ファイルを消し、古いファイルはそのまま残ります。置き換えるまでは古いファイルと新しいファイルの両方がディスクにあるので、そのぶんの空きが要ります（容量の上限を超えるときは`storage_capacity_exceeded`で断ります）。作り直した索引には、計算中に記録した行動・会話の判断がなく、分岐した子Branchも分岐元を読まずに自分の行をすべて持ちます。判断まで戻すには、Runを計算し直します。版6を知らない前のコードは、版6の索引を`history_index_version_unsupported`で断ります。

no-op childは親に記録された既存Provider応答を、cache/fallback/費用metadataごと引き継いで再生します。明示的に通常継続を依頼した場合は設定済みrouterで記録範囲の先へ進められますが、過去seekがProviderへfallbackすることはありません。承認済み介入は将来分の継承response replayを無効にし、既に発生した予算記録と過去の承認済みtapeは保持します。

## 旧モデルのRun（閲覧のみ）

Checkpointは計算に使ったモデルの版を`model_versions`に記録します（モデル全体の版`engine:model`とRNGの`engine:rng_algorithm`を含みます。仕組みは[architecture.md](architecture.md)）。Branchの先頭のCheckpointの版が今のモデルと違うRunは閲覧のみです。

- **読める：** frame（`GET …/timeline?minute=M`）、replay-frames、比較、履歴（`people`、`households`、索引の検索）、`first_divergence`、互換性（`GET …/compatibility`）、PDFレポート、CLIの`frame`と`history`。保存した時点のCheckpointを、hashを確かめて閲覧用に復元します（`ProjectStore.load_view_checkpoint`、`TimelineStore.view_at`）。
- **保存の間の時刻：** 今のモデルで計算し直さず、直前の保存の時点を表示します（D2）。frameの`minute`は表示している保存の時点で、`saved_point_rounding`に求めた時刻（`requested_minute`）、表示した時点（`saved_minute`）、日本語の説明が入ります。`model_view`には記録した版、今の版、違う項目、日本語の説明が入ります。履歴の行は`exact: false`になり、`coverage`は`saved_points_of_an_older_model_run`です。`iter_worlds`は求めた1分ごとに1つの世界を返し、その世界は直前の保存の時点の状態です（`requested_minute()`で求めた時刻が分かります）。PDFレポートは保存の時点を選ぶよう求めます（`pdf_saved_point_required`）。画面（`/demo`のProject、`/demo-v2`）は説明の文を表示し、時刻の表示は保存の時点に戻ります。
- **止める：** 続きの計算（`POST R/resume`、CLIの`resume`）、分岐（`fork`、CLIの`fork`）、介入の確認と適用（intervention-preview、EffectProgram）、`TimelineStore.seek`。HTTPは409、`code`は`run_model_view_only`、`error`は「このRunは旧モデル（版 1）で計算されました。今のモデルは版 2 です。閲覧のみです。…」のような文です。Webの計算開始とProjectの分岐は、workerを起動する前に止めます。保存したファイルは変えません。旧モデルのdemoのjobは、server再起動のときに`failed`として残し、Branchを復元しません。
- 版の違いが、全体の版ではなくPolicyや状態契約の署名（`model_suite`など）だけのときも同じです。今のコードが署名できない設定も旧モデルとして閲覧します。
- 独立した待ち行列ができる前の保存（列の旧方式）と、情報の状態のschema 1を含む保存は、版が同じでも閲覧のみです（`LEGACY_SHAPES_TICKABLE`は`False`。tickの規則からPythonを消したため）。閲覧では今までどおり移行して読みます。

## ブラウザーのProject記録と1つのファイル（#265）

ブラウザー版（#132の段階6）は、Project・Run・Branchの記録をOPFSのフォルダーに置き、画面が呼ぶ`GET /api/v1/projects`、`POST /api/v1/projects`、`GET {project}`、`GET {project}/runs`、`GET {project}/runs/{run}`、`GET {project}/runs/{run}/evidence`に、Pythonのserverと同じ形のJSONで答えます（`native/wasm/js/project_store.mjs`）。VM（Pythonの経路）のRunのフォルダーは変換しません。

| 置き場所（Projectのフォルダーから） | 中身 |
|---|---|
| `project.json` | `schema_version`、`project_id`、`archived` |
| `scenarios/<scenario>/v<n>.json` | Scenarioの記録（`schema_version`、`scenario_id`、`scenario_version`、`scenario_hash`、`approved`）。Scenario自体は持たない |
| `scenarios/<scenario>/v<n>.scenario.json` | Pythonが書いたScenarioの正準JSONのbytes。sha256が`scenario_hash` |
| `runs/<run>/run.json` | Runの記録（Pythonの`RunReference`の欄と`status`、`archived`） |
| `runs/<run>/evidence.json` | Pythonのserverが`runs/{run}/evidence`に返すJSONのbytes（`evidence_context`と、その道が足す一覧） |
| `runs/<run>/branches/<branch>/branch.json` | Branchの記録（Pythonの`BranchReference`の欄と`status`） |
| `runs/<run>/branches/<branch>/journal.jsonl`、`data.pack` | Branchの保存したRun（#252、`run_store.mjs`） |

記録は、鍵を並べたJSONと改行です。ScenarioとevidenceはPythonが書いたbytesのまま持ち、答えるときもそのまま埋め込みます（JSで読み直すと`1.0`が`1`になるため）。案A（#244）のあいだは、世界を作るのも、Scenarioとevidenceを作るのもPythonです。

Pythonの欄のうち、ブラウザーで意味が変わるもの：

| 欄 | ブラウザーで入るもの |
|---|---|
| Branchの`checkpoint_hash` | 先頭（`computed_until`の世界）を記録した目録の行のsha256。保存したRunの最後の`end`の行で、分とdigestを持つ。計算前は`null`。画面は先頭が変わったかどうかにだけ使う |
| Runの`status` | `run.json`に持つ（Pythonは`status.json`に分ける） |
| Projectの一覧の`storage` | `bytes`と`files`は置き場のファイルの合計、`limit_bytes`は呼ぶ側が渡す値（ページでは`navigator.storage.estimate()`の`quota`） |
| Runの`last_fork` | 返さない（分岐は#267） |

誤りは`ProjectStoreError`で、`message`はPythonの`StorageError`と同じ文（あれば）、`code`はその符号です。

1つのファイル（`sheltersim-project-bundle`、版1、拡張子`.shelterproj`）は、Projectのフォルダーのファイルをそのまま並べたものです。先頭から、16 bytesの印（`0x89`、`SHELTERSIM-PRJ`、`0x0a`）、版（4 bytes、big-endian）、目録の長さ（4 bytes）、目録のsha256（32 bytes）、目録（JSON：`format`、`version`、`project_id`、作った道具の`source`、各entryの`path`・`length`・`sha256`）、各entryのbytesの順です。Branchの保存したRunは、目録のpieceの種類を読まずにファイルごと運ぶので、#251が足す途中の世界もそのまま入ります。

読み込み（`importProject`）は、印、版、目録とentryのsha256、記録どうしの関係（Scenarioのhash、RunのScenario、Branchの親）を確かめます。続けて、各Branchの保存したRunを、メモリーの中に置いたentryから読み戻し、digest（`readAndVerify`）と記録の先頭を確かめます。すべて合ってから書きます。どれかが合わなければ誤りにし、何も書きません。`project.json`は最後に書くので、途中で止まったProjectは一覧に出ません。同じIDのProjectがあれば断ります。`replace`では、ファイルを確かめ終えてから古いものを消して書きます。そのため、確かめで断られたときは古いProjectがそのまま残ります（#273）。消した後に書き込みそのものが失敗したとき（容量が足りないなど）は、古いProjectは戻らず、新しいものも残りません。違う版のファイルは「版Nは読めない」という誤りにします。

| 関数 | 使う所 |
|---|---|
| `memoryTree()`、`opfsTree(directory)` | 置き場 |
| `createProject`、`saveScenario`、`createRun`、`setRunStatus`、`writeBranch`、`computeBranch`、`branchBackend` | 記録を作る、Branchを計算して残す |
| `projectsResponse`、`projectResponse`、`runsResponse`、`runResponse`、`evidenceResponse` | 道の答え（JSONの文字列） |
| `exportProject`、`importProject`、`readBundle`、`packBundle` | 1つのファイル |

計算済みの例は`scripts/build_browser_sample.py`で作ります（[development.md](development.md#ブラウザー向けのwasm試作)）。

## ブラウザーの時系列・レポート・LLMの状態（#266）

ブラウザー版は、Branchの`GET …/history?kind=kpis`、`GET …/report`、`GET …/llm-status`に、同じRunにPythonのserverが返すのと同じbytesのJSONで答えます（`native/wasm/js/api_answers.mjs`）。

| 関数 | 答える道 |
|---|---|
| `historyResponse({wasm, tree, projectId, runId, branchId, query, cache})` | `history`。`query`は`?`の後ろの文字列（Pythonの`parse_qs`と同じ読み方。空の値は無い扱い）。`cache`は`historyCache()`で、読み戻した標本をBranchの先頭ごとに持ちます |
| `reportResponse({wasm, tree, projectId, runId, branchId, query})` | `report` |
| `llmStatusResponse({tree, projectId, runId, branchId, query})` | `llm-status` |
| `errorAnswer(error)` | 誤りを`{status, body}`にします。`ApiError`はその`status`、`ProjectStoreError`はserverの`StorageError`と同じ409と`code`、そのほかは500 |

どれも答えのJSONの文字列を返し、断るときは`ApiError`（`status`、`message`がserverの`error`、`code`）を投げます。serverと同じく、問い合わせの欄の誤り（知らない欄、同じ欄が2つ）は409の`invalid_query_fields`、契約に合わない値は400の`invalid_input`、計算した範囲の外は409の`history_outside_computed_range`、合わないcursorは409の`stale_or_foreign_cursor`です。

時系列は、Pythonの履歴の索引ではなく、Branchの保存した指標の標本（#252）から作ります。行は索引と同じで（`MetricSnapshot`の欄から`source_ids_json`を除き、資源の台帳の列、`time_semantics`、`record_kind`、`run_id`、`branch_id`を足したもの）、`start`から`end`までの`start + k*step`の分を、cursorの後から`limit`件返します。資源の台帳の列は、標本の形`sheltersim-browser-run-v3`から標本が持ちます（[development.md](development.md#ブラウザー向けのwasm試作)）。

Pythonの欄のうち、ブラウザーで意味が変わるもの：

| 欄・動き | ブラウザーで入るもの |
|---|---|
| 時系列の`anchor_hash`とcursorの`anchor` | Branchの先頭（記録の`checkpoint_hash`）。cursorのほかの中身（`scope`、分、key）はPythonと同じ |
| 時系列の`indexed_through_minute` | Branchの`computed_until`（保存したRunは先頭まで標本を持つので、索引の遅れはありません） |
| 時系列の`first_indexed_checkpoint_minute` | Branchの`fork_minute`（保存したRunの始まり） |
| `kind`が`kpis`以外の時系列 | 409、`browser_unsupported`（範囲などのserverの確かめの後） |
| `v1`・`v2`の形で保存したBranchの時系列 | 409、`browser_run_outdated`（標本が資源の台帳の列を持たないため。計算し直します） |
| 保存したRunのないBranch | `anchor_hash`と`indexed_through_minute`と`first_indexed_checkpoint_minute`が`null`、行なし |
| レポート | serverは計算が終わったときに書きます。ブラウザーは、終わったBranchで最初に聞かれたときに、保存したRunの最後の分から作り（最後の点に戻すので、100人・7日のNodeで約4秒）、Branchのフォルダーに`report.json`として残します。終わっていなければ`{"status":"not_generated"}` |
| LLMの状態の`checkpoint_hash` | Branchの先頭。`mode`は`offline`、記録はありません |

レポートは、画面のofflineのRun（`scenario-approve`から作り、`SHELTERSIM_LLM_MODE=offline`で計算したもの）でserverが書くものです。評価の依頼はofflineで規則のfallbackになり（`rule_fallback`）、最後の指標の標本、最後の100件の記録のうち重要なもの20件、Scenarioから決まる仮定のID、世界のdigest（`state_hash`）を持ちます。`execution_mode: offline`で作ったRun（実験用、`offline_experiment_no_paid_review`）の答えではありません。

Pythonの索引のcursorは、問い合わせの内容ではなく、Branchの行の範囲（`HistoryIndex.search`で、問い合わせのdigestと同じ名前`scope`に後から入るSQLの条件）と先頭に結び付いています。そのため、同じBranchの別の問い合わせのcursorも受け付けます。ブラウザーも同じにしています。

`native/wasm/tests/node_api_answers.mjs`が、`native/core/tests/data/api_answers.json`のRunで、serverの答えと同じbytesになることを確かめます。
