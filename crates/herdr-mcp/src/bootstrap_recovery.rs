use crate::locale::Locale;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryReason {
    MissingCanonicalDevice,
    ExistingFleetMaterial,
    StaleJournal,
}

pub(crate) fn refusal(reason: RecoveryReason, language: Locale) -> String {
    match reason {
        RecoveryReason::MissingCanonicalDevice => language
            .text(
                "Bootstrap is stranded after secret provisioning: the canonical device is missing, so the device registry state is unproven. No secret reprovisioning or second-device enrollment was attempted. Preserve the journal and credentials; continue through an enrolled computer's `herdr-mcp worker pair`, or perform manual account/Worker review. Do not delete the journal or rewrite secrets.",
                "Bootstrap 在密钥供给后处于悬置状态：缺少 canonical device，设备登记表状态未证明为空。未重新供给密钥，也未创建第二个设备。请保留 journal 与凭据，通过已登记电脑执行 `herdr-mcp worker pair`，或进行人工账户/Worker 核对。不要删除 journal 或改写密钥。",
                "シークレット供給後の bootstrap は停止しています。canonical device がないため、デバイス台帳が空であることを証明できません。シークレットの再供給や 2 台目の登録は行っていません。journal と資格情報を保持し、登録済みコンピュータの `herdr-mcp worker pair`、または手動のアカウント・Worker 確認に進んでください。journal を削除したり、シークレットを書き換えたりしないでください。",
            )
            .to_owned(),
        RecoveryReason::ExistingFleetMaterial => language
            .text(
                "An existing Herdr fleet secret was found, but this computer has no proven local enrollment for it. This is stranded fleet material, not proof that a first-device registry is empty. No Worker, secret, or enrollment mutation was attempted. Preserve the Worker and secrets; continue from an enrolled computer with `herdr-mcp worker pair`, or perform manual account/Worker review. Do not delete or rewrite fleet secrets.",
                "检测到已有 Herdr fleet 密钥材料，但本机没有可证明的本地 enrollment。这是悬置的 fleet material，不能证明首台设备登记表为空。未修改 Worker、密钥或 enrollment。请保留 Worker 与密钥，通过已登记电脑执行 `herdr-mcp worker pair`，或进行人工账户/Worker 核对。不要删除或改写 fleet 密钥。",
                "既存の Herdr fleet シークレットが見つかりましたが、このコンピュータにその登録を証明できるローカル enrollment がありません。これは stranded fleet material であり、初回デバイス台帳が空である証明ではありません。Worker、シークレット、enrollment は変更していません。Worker とシークレットを保持し、登録済みコンピュータで `herdr-mcp worker pair` を実行するか、手動でアカウント・Worker を確認してください。fleet シークレットを削除または書き換えないでください。",
            )
            .to_owned(),
        RecoveryReason::StaleJournal => language
            .text(
                "The bootstrap journal does not match this release, Cloudflare account, or Worker. Recovery is stopped until the upgrade is completed or rolled back; no Worker, secret, or enrollment mutation was attempted. Preserve the journal for review and do not delete it or rewrite secrets.",
                "bootstrap journal 与当前 release、Cloudflare 账户或 Worker 不匹配。恢复已停止，必须先完成或回滚升级；未修改 Worker、密钥或 enrollment。请保留 journal 供核对，不要删除 journal 或改写密钥。",
                "bootstrap journal はこの release、Cloudflare アカウント、または Worker と一致しません。アップグレードを完了またはロールバックするまで復旧を停止しています。Worker、シークレット、enrollment は変更していません。確認のため journal を保持し、削除したりシークレットを書き換えたりしないでください。",
            )
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_canonical_device_stays_fail_closed() {
        let message = refusal(RecoveryReason::MissingCanonicalDevice, Locale::En);
        assert!(message.contains("canonical device is missing"));
        assert!(message.contains("device registry state is unproven"));
        assert!(message.contains("No secret reprovisioning"));
        assert!(message.contains("herdr-mcp worker pair"));
        assert!(message.contains("Do not delete the journal"));
    }

    #[test]
    fn existing_fleet_material_requires_pairing_or_manual_review() {
        let message = refusal(RecoveryReason::ExistingFleetMaterial, Locale::ZhCn);
        assert!(message.contains("已有 Herdr fleet 密钥材料"));
        assert!(message.contains("不能证明首台设备登记表为空"));
        assert!(message.contains("herdr-mcp worker pair"));
        assert!(message.contains("不要删除或改写 fleet 密钥"));
    }

    #[test]
    fn recovery_exit_text_contains_no_secret_values_or_identifiers() {
        for reason in [
            RecoveryReason::MissingCanonicalDevice,
            RecoveryReason::ExistingFleetMaterial,
            RecoveryReason::StaleJournal,
        ] {
            let message = refusal(reason, Locale::En);
            assert!(!message.contains("dev_"));
            assert!(!message.contains("LINK_SHARED_SECRET"));
            assert!(!message.contains("STATIC_MCP_BEARER_SECRET"));
            assert!(!message.contains("account_id"));
        }
    }

    #[test]
    fn stale_journal_requires_upgrade_completion_or_rollback() {
        let message = refusal(RecoveryReason::StaleJournal, Locale::En);
        assert!(message.contains("does not match this release"));
        assert!(message.contains("completed or rolled back"));
        assert!(message.contains("no Worker, secret, or enrollment mutation"));
        assert!(message.contains("do not delete it"));
    }
}
