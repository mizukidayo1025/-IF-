# 審査用VMへのデプロイ

審査員などにブラウザーから使ってもらうための、1台のVMへのデプロイ手順です。一式は`deploy/vm/`にあります。

## 構成

- Webサーバーは認証を持たず、同じプロセスを開いた人どうしで状態を共有します。そのため、利用者ごとに分ける「環境」（slot）を用意します。slotごとに`sheltersim web`のプロセス（systemdの`sheltersim@<n>`）と、`fixtures/demo_replay`をコピーしたデータ（`/var/lib/sheltersim/slot<n>`）を分けます。既定は6つです。
- Caddyが`https://s<n>.<IPのドットをハイフンにしたもの>.sslip.io`を各slotへ中継し、証明書を自動で取ります。アプリは`/`を従来の画面（`/demo`）へ送るので、slotの`/`はCaddyが新しい画面（`/demo-v2`）へ転送します。従来の画面は入口ページのリンクか`/demo`で開けます。入口のページ（`https://<IP>.sslip.io/`）とすべてのslotに、共通のBasic認証を1つかけます。
- アプリは、`setup.sh`を取り出したcommitから`/opt/sheltersim/app`へcloneし、`/opt/sheltersim/venv`へ入れます。Rustのkernel（`native/`）もVM上で作って入れます。そのcommitはGitHubへpush済みである必要があります。別のcommitを入れるときは`sudo REF=<commit> bash deploy/vm/setup.sh`のように実行します。
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

## 運用

| したいこと | コマンド |
|---|---|
| slotの状態を見る | `systemctl status 'sheltersim@*'` |
| slotのログを見る | `journalctl -u sheltersim@1 -f` |
| slotを配布時のデータに戻す | `sudo /opt/sheltersim/reset-slot.sh 1` |
| アプリを別のcommitに更新する | `cd sheltersim-kit && git pull && sudo bash deploy/vm/setup.sh` |
| 使わない間に費用を止める | Cloud Shellで`gcloud compute instances stop sheltersim --zone=asia-northeast1-b`（固定IPとディスクの費用は残ります） |
| すべて消す | `gcloud compute instances delete sheltersim --zone=asia-northeast1-b`、`gcloud compute addresses delete sheltersim-ip --region=asia-northeast1`、`gcloud compute firewall-rules delete sheltersim-web` |

入口のページは、同じ番号のslotを複数人で使わないよう案内します。slotの割り当ては利用者に任せており、使用中かどうかを確かめる仕組みはありません。
