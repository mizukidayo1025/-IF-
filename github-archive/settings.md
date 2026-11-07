# GitHub設定の取得・反映状況

取得した公開メタデータの比較です。`destination_snapshot` はIssue再作成前の取得値です。設定変更APIが利用できないため、この作業では設定の書き換えは実施していません。

| 項目 | 元 | 複製先の取得値 | 一致 |
|---|---|---|---|
| description | "避難者の行動・心理状態・感情伝播まで再現し、途中介入・A/B比較・AI分析を通じて、避難所運営の「もしも」を事前に検証できるシミュレーター" | "避難者の行動・心理状態・感情伝播まで再現し、途中介入・A/B比較・AI分析を通じて、避難所運営の「もしも」を事前に検証できるシミュレーター  Resources" | 未反映 |
| homepage | "" | null | はい |
| default_branch | "main" | "main" | はい |
| has_issues | true | true | はい |
| has_wiki | true | true | はい |
| has_projects | true | true | はい |
| has_discussions | false | false | はい |
| allow_squash_merge | true | true | はい |
| allow_merge_commit | true | true | はい |
| allow_rebase_merge | true | true | はい |
| allow_auto_merge | false | false | はい |
| delete_branch_on_merge | false | false | はい |
| allow_update_branch | false | false | はい |
| web_commit_signoff_required | false | false | はい |
| squash_merge_commit_message | "COMMIT_MESSAGES" | "COMMIT_MESSAGES" | はい |
| squash_merge_commit_title | "COMMIT_OR_PR_TITLE" | "COMMIT_OR_PR_TITLE" | はい |
| merge_commit_message | "PR_TITLE" | "PR_TITLE" | はい |
| merge_commit_title | "MERGE_MESSAGE" | "MERGE_MESSAGE" | はい |
| topics | [] | [] | はい |

- 公開ブランチ一覧では全8ブランチが `protected: false`。ルールセット一覧は空です。保護設定の詳細APIは接続アプリにより403となり、取得できません。
- ラベル・マイルストーンの付与は全Issue・PRで0件。定義一覧は現在のツールでは取得できません。
- Releaseは0件。Wikiは有効ですが、WikiのGit URLへの読み取りはRepository not foundとなり、コンテンツを取得できませんでした。
- Actions権限、Environments、Secrets、Variables、Webhooks、Deploy keys、GitHub Apps、協力者・組織チーム、Projectsなどの管理設定は未取得・未反映です。Secrets値はGitHub APIでも読み戻せません。
- `apply-public-settings.py` は公開の基本設定・Topics・ラベル定義・マイルストーン定義について、認証済みghが利用できる環境で差分確認・反映するための補助手順です。この作業では未実行です。管理設定全体を複製するものではありません。
