# 審査用VMへのデプロイ

審査員などにブラウザーから使ってもらうための、1台のVMへのデプロイ手順です。一式は`deploy/vm/`にあります。

## 構成

- Webサーバーは認証を持たず、同じプロセスを開いた人どうしで状態を共有します。そのため、利用者ごとに分ける「環境」（slot）を用意します。slotごとに`sheltersim web`のプロセス（systemdの`sheltersim@<n>`）と、`fixtures/demo_replay`をコピーしたデータ（`/var/lib/sheltersim/slot<n>`）を分けます。既定は6つです。各slotには、計算済みの例（下の「計算済みの例」）のコピーも入れます。
- Caddyが`https://s<n>.<IPのドットをハイフンにしたもの>.sslip.io`を各slotへ中継し、証明書を自動で取ります。審査では新しい画面（`/demo-v2`）だけを使うため、slotの`/`と従来の画面（`/demo`、`/admin`、`/index.html`）は、Caddyが`/demo-v2`へ転送します。転送先には元のquery（`?project=...&run=...`）をそのまま付けます。APIと新しい画面のファイルはそのまま通します。入口のページ（`https://<IP>.sslip.io/`）とすべてのslotに、共通のBasic認証を1つかけます。
- アプリは、`setup.sh`を取り出したcommitから`/opt/sheltersim/app`へcloneし、`/opt/sheltersim/venv`へ入れます。Rustのkernel（`native/`）もVM上で作って入れます。そのcommitはGitHubへpush済みである必要があります。別のcommitを入れるときは`sudo REF=<commit> bash deploy/vm/setup.sh`のように実行します。
- slotごとの保存データの上限（`sheltersim web --project-storage-gb`）は、ディスクの容量から15 GBと計算済みの例の元（`/var/lib/sheltersim/sample`）の大きさを引き、slotの数で割った値です。すべてのslotが上限まで使ってもディスクがあふれません。slotに入れた計算済みの例のコピーは、そのslotの上限に含まれます。`STORAGE_GB=<GB>`で指定することもできます。ディスクを増やした後は、`setup.sh`を実行し直すと上限も増えます。
- LLMの利用台帳（`XDG_STATE_HOME`）は全slotで1つを共有するため、既定50ドルの上限がVM全体にかかります。応答テープ（`SHELTERSIM_LLM_CACHE_PATH`）はslotごとです。

## VMを作る（Cloud Shell）

GCPコンソールでCloud Shellを開き、プロジェクトを確かめてから実行します（`gcloud config get-value project`）。

~~~shell
REGION=asia-northeast1
ZONE=asia-northeast1-b
gcloud services enable compute.googleapis.com
gcloud compute addresses create sheltersim-ip --region=$REGION
gcloud compute firewall-rules create sheltersim-web \
  --allow=tcp:80,tcp:443 --target-tags=sheltersim-web
gcloud compute instances create sheltersim --zone=$ZONE \
  --machine-type=e2-standard-8 \
  --image-family=ubuntu-2404-lts-amd64 --image-project=ubuntu-os-cloud \
  --boot-disk-size=50GB --boot-disk-type=pd-balanced \
  --address=sheltersim-ip --tags=sheltersim-web
gcloud compute addresses describe sheltersim-ip --region=$REGION --format='value(address)'
gcloud compute ssh sheltersim --zone=$ZONE
~~~

slotを減らすなら、小さいマシン（`e2-standard-4`など）でもかまいません。各slotのメモリーの上限は`sheltersim@.service`の`MemoryMax`です。

## 入れる

VMにSSHで入り、リポジトリを取り出して`setup.sh`を実行します。VMの外部IP（上で作った固定IP）は、GCPのメタデータから自動で読みます。GCP以外で動かすときや読めないときは、`setup.sh <IP>`のように引数で渡します。

~~~shell
git clone https://github.com/mizukidayo1025/HinanjoIF-Shelter_Sim-.git sheltersim-kit
sudo bash sheltersim-kit/deploy/vm/setup.sh
~~~

最後に、入口のURLとBasic認証の利用者名・パスワードが表示されます。パスワードは`/etc/sheltersim/basic-auth`（rootだけが読めます）に残ります。もう一度実行しても、APIキー、パスワード、slotのデータは保たれます。

## LLMを使う

APIを呼ぶにはプロジェクトオーナーの承認が必要です（[llm-integration.md](llm-integration.md)）。承認を得たら、VMの上で自分でAPIキーを書き込みます。キーをチャットやGitに貼りません。

~~~shell
sudo nano /etc/sheltersim/env    # ANTHROPIC_API_KEY= の後ろにキーを書く
sudo systemctl restart 'sheltersim@*'
~~~

キーがなければ、LLMを使わない処理（offline）で動きます。モデル、上限、同時実行数も同じファイルで変えます。

## 計算済みの例

審査員が計算を待たずに新しい画面を試せるよう、計算済みのProject `sample`を各slotに入れます。中身は`scripts/build_project_sample.py`が作ります。

- Scenarioは、新しい画面の「例文を使う」が入れる文章（墨田区、地震、避難者100人、7日間）から、画面と同じ`scenario-preview`（offline parser）で作った合成のScenarioです。標準の合成施設（トイレ`toilet_01`〜`toilet_03`と`accessible_toilet_01`など）と人の座標を持つため、地図に人が表示されます。seedは2026です。地域の公開データの調べは、結果が日によって変わるため行いません（Scenarioの記録にその旨が残ります）。
- そのRunを最後まで計算した`main`と、4日目14:00（5160分）に「Day 4 14:00 toilet_01 outage」を承認して分岐し、最後まで計算した分岐です。従来の画面の事前計算replay（`fixtures/demo_replay`）と同じ種類の比較を、新しい画面が読めるProjectの形で保存したものです。実際の避難所の記録や予測ではありません。
- 新しい画面と同じProjectAPIの操作（Scenarioのプレビューと承認、終わりまでの計算、offline parserでの介入プレビュー、明示承認での適用、分岐の計算）で作ります。LLMはofflineに固定し、APIキーを渡さず、共有の利用台帳（`XDG_STATE_HOME`）にも触れません。同じアプリのcommitなら、何度作っても結果のhashは同じです。
- `setup.sh`は、`sheltersim`利用者として`/var/lib/sheltersim/sample`に作ります（`sample-build.json`にRun ID・分岐・hash・大きさを残します）。作り直すのは、今作ると結果が変わるときだけです。`build_project_sample.py --check`が計算せずに、作り方（文章・ID・分岐の時刻・介入）、文章から作るシナリオのhash、導入済みのモデルの署名とCheckpointに記録した署名を比べ、履歴の索引が今の版かを確かめます。アプリを更新しても結果が変わらず、索引の版も同じなら、作り直しません（索引の版5より前に作った例は、索引が古いので作り直します。#130、#138）。介入の文章をオフラインで読む規則を変えたときは比べないので、`SAMPLE=rebuild`を付けて作り直します。その後、`sample`のないslotにだけコピーします。すでにあるslotの`sample`は、審査員が足した分岐を消さないよう置き換えません。作り直した例に替えるときは、そのslotを`reset-slot.sh`で戻します。作らないときは`sudo SAMPLE=0 bash sheltersim-kit/deploy/vm/setup.sh`のように実行します（元も消します。slotのコピーは残ります）。
- 入口のページは、新しく計算するときの目安（1日分と7日分）を出します。値は、このVMで計算済みの例を作ったときの時間（`sample-build.json`の`build_seconds`）を、計算した分数（元の7日分と、分岐の時刻から後の分）で割って求め、切り上げます。例がないときは、決まった目安（1日分約25秒）を出します。
- 入口のページは、各slotの「環境 n」の隣に「計算済みの例をすぐ見る」を置きます。リンク先は`/demo-v2?project=sample&run=<Run ID>&branch=main#simulation`（`sample-build.json`の`open_path`）で、元のシナリオの1日目00:00の画面が開きます。比較の画面で「比較を更新」を押すと、トイレが故障した分岐との比較が出ます。
- 70783d6（履歴の索引の圧縮の後）で作ったときは、大きさ約3.6 GB（履歴の索引`history.sqlite`が約2.8 GB、Checkpointが約0.6 GB）、計算に約4分、メモリーは最大約3.5 GBでした。元と6つのslotのコピーで、ディスクを約25 GB使います。たとえば100 GBのディスクでは、slotの上限は(100−15−4)/6 = 13 GBになり、そのうち約3.6 GBを例のコピーが使います。上の手順どおり50 GBのディスクで作ると、slotの上限は5 GBで、例のほかに新しい7日分のRunを計算する余裕がありません。ディスクを増やすか、`SAMPLE=0`にします。slotごとにコピーするのは、計算や分岐で書き換わるファイル（履歴の索引、lock）をslotの間で共有しないためです。

## 運用

| したいこと | コマンド |
|---|---|
| slotの状態を見る | `systemctl status 'sheltersim@*'` |
| slotのログを見る | `journalctl -u sheltersim@1 -f` |
| slotを配布時のデータ（計算済みの例を含む）に戻す | `sudo /opt/sheltersim/reset-slot.sh 1` |
| 計算済みの例を作り直す | `sudo SAMPLE=rebuild bash sheltersim-kit/deploy/vm/setup.sh`（slotへは`reset-slot.sh`で入れ直します） |
| アプリを別のcommitに更新する | `cd sheltersim-kit && git pull && sudo bash deploy/vm/setup.sh` |
| 使わない間に費用を止める | Cloud Shellで`gcloud compute instances stop sheltersim --zone=asia-northeast1-b`（固定IPとディスクの費用は残ります） |
| すべて消す | `gcloud compute instances delete sheltersim --zone=asia-northeast1-b`、`gcloud compute addresses delete sheltersim-ip --region=asia-northeast1`、`gcloud compute firewall-rules delete sheltersim-web` |

入口のページは、同じ番号のslotを複数人で使わないよう案内します。slotの割り当ては利用者に任せており、使用中かどうかを確かめる仕組みはありません。
