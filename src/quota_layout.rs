//! Pure ordering logic for the quota cards, kept platform-neutral so its
//! regression tests run on every development machine.
use crate::quota::{ProviderId, ProviderQuota};
use eframe::egui;

/// Stable per-provider slot for egui widget ids (ProviderId is not Hash).
pub(crate) fn provider_slot(provider: ProviderId) -> u8 {
    match provider {
        ProviderId::Glm => 0,
        ProviderId::Agy => 1,
        ProviderId::Codex => 2,
        ProviderId::Claude => 3,
        ProviderId::Qwen => 4,
    }
}

/// Sort visible quota cards into the persisted display order. Providers
/// missing from the order keep their snapshot order at the end.
pub(crate) fn apply_quota_order(providers: &mut [ProviderQuota], order: &[ProviderId]) {
    providers.sort_by_key(|quota| {
        order
            .iter()
            .position(|provider| *provider == quota.id)
            .unwrap_or(order.len())
    });
}

/// Stable egui widget id per visible card. One provider can show several
/// account cards, so the account slot is part of the identity.
pub(crate) fn card_widget_id(provider: ProviderId, account_id: Option<&str>) -> egui::Id {
    egui::Id::new(("quota_card", provider_slot(provider), account_id))
}

/// Move `provider` so it lands at `visible_index` among `visible`. `visible`
/// lists one entry per visible card without the dragged provider, so a
/// provider with several account cards appears several times in a row. The
/// order itself keeps one entry per provider: a drop inside such a group snaps
/// behind the whole group, and a drop past the last card lands behind the last
/// card's group. The target anchor is resolved only after the dragged provider
/// has been removed, so positions below its old slot do not shift.
pub(crate) fn reorder_quota_order(
    order: &mut Vec<ProviderId>,
    provider: ProviderId,
    visible: &[ProviderId],
    visible_index: usize,
) {
    if visible.is_empty() {
        return;
    }
    order.retain(|entry| *entry != provider);
    let order_index = if visible_index >= visible.len() {
        visible
            .last()
            .and_then(|last| order.iter().position(|entry| *entry == *last))
            .map(|index| index + 1)
            .unwrap_or(order.len())
    } else {
        let target = visible[visible_index];
        let target_index = order
            .iter()
            .position(|entry| *entry == target)
            .unwrap_or(order.len());
        let inside_group = visible_index > 0 && visible[visible_index - 1] == target;
        if inside_group {
            target_index + 1
        } else {
            target_index
        }
    };
    let insert_at = order_index.min(order.len());
    order.insert(insert_at, provider);
}

/// The reset that ends a provider's depletion, for the minimized card row:
/// a drained weekly budget is the real blocker, otherwise the session reset.
pub(crate) fn minimized_reset(quota: &ProviderQuota) -> Option<(&str, bool)> {
    if !quota.provider_is_throttled() {
        return None;
    }
    if quota.id == ProviderId::Codex {
        if quota.explicit_provider_throttle {
            return None;
        }
        if quota.week_is_throttled() {
            return quota.week_reset.as_deref().map(|reset| (reset, false));
        }
        if quota.five_hour_is_throttled() {
            return quota.five_hour_reset.as_deref().map(|reset| (reset, true));
        }
        return None;
    }
    if quota.week_percent == Some(0) {
        quota.week_reset.as_deref().map(|reset| (reset, false))
    } else {
        quota.five_hour_reset.as_deref().map(|reset| (reset, true))
    }
}

/// Picks the reset text of a minimized card row so it stays short of the status LED.
/// The full text is used when it fits. Otherwise the part before the first " ("
/// (the date suffix) is used. The caller clips whatever still exceeds the space.
pub(crate) fn fit_reset_text(full: &str, available: f32, measure: impl Fn(&str) -> f32) -> String {
    if measure(full) <= available {
        return full.to_string();
    }
    match full.find(" (") {
        Some(index) => full[..index].to_string(),
        None => full.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order() -> Vec<ProviderId> {
        vec![ProviderId::Claude, ProviderId::Glm, ProviderId::Agy, ProviderId::Codex]
    }

    #[test]
    fn apply_quota_order_sorts_by_persisted_order_and_appends_unknown() {
        let glm = ProviderQuota::new(ProviderId::Glm, "GLM", "Z.ai", None, None, Some(90), Some("01.10.".into()));
        let agy = ProviderQuota::new(ProviderId::Agy, "AGY", "Google", Some(73), Some("19:45".into()), Some(85), Some("137h".into()));
        let codex = ProviderQuota::new(ProviderId::Codex, "Codex", "OpenAI", Some(20), Some("22:00".into()), Some(58), Some("30.10.".into()));
        let mut providers = vec![glm, agy, codex];

        apply_quota_order(&mut providers, &[ProviderId::Codex, ProviderId::Glm]);
        assert_eq!(
            providers.iter().map(|quota| quota.id).collect::<Vec<_>>(),
            vec![ProviderId::Codex, ProviderId::Glm, ProviderId::Agy]
        );
    }

    #[test]
    fn drop_on_own_slot_keeps_the_order_unchanged() {
        let visible_without = vec![ProviderId::Claude, ProviderId::Agy, ProviderId::Codex];
        let mut full = order();
        // GLM dropped onto its own slot: the marker aims between Claude and Agy.
        reorder_quota_order(&mut full, ProviderId::Glm, &visible_without, 1);
        assert_eq!(full, order());
    }

    #[test]
    fn downward_drop_lands_exactly_at_the_marker() {
        let visible_without = vec![ProviderId::Glm, ProviderId::Agy, ProviderId::Codex];
        let mut full = order();
        // Claude dropped between Agy and Codex (in front of Codex).
        reorder_quota_order(&mut full, ProviderId::Claude, &visible_without, 2);
        assert_eq!(
            full,
            vec![ProviderId::Glm, ProviderId::Agy, ProviderId::Claude, ProviderId::Codex]
        );
    }

    #[test]
    fn upward_drop_moves_the_provider_before_the_target() {
        let visible_without = vec![ProviderId::Claude, ProviderId::Glm, ProviderId::Agy];
        let mut full = order();
        // Codex dropped in front of GLM.
        reorder_quota_order(&mut full, ProviderId::Codex, &visible_without, 1);
        assert_eq!(
            full,
            vec![ProviderId::Claude, ProviderId::Codex, ProviderId::Glm, ProviderId::Agy]
        );
    }

    #[test]
    fn drop_at_the_end_appends_after_the_last_visible_card() {
        let visible_without = vec![ProviderId::Claude, ProviderId::Glm, ProviderId::Agy];
        let mut full = order();
        reorder_quota_order(&mut full, ProviderId::Codex, &visible_without, visible_without.len());
        assert_eq!(
            full,
            vec![ProviderId::Claude, ProviderId::Glm, ProviderId::Agy, ProviderId::Codex]
        );
    }

    #[test]
    fn minimized_reset_prefers_the_weekly_blocker() {
        let week_depleted = ProviderQuota::new(
            ProviderId::Claude,
            "Claude",
            "Anthropic",
            Some(100),
            Some("21:00".into()),
            Some(0),
            Some("31.10. (02:08)".into()),
        );
        assert_eq!(
            minimized_reset(&week_depleted),
            Some(("31.10. (02:08)", false))
        );

        let session_depleted = ProviderQuota::new(
            ProviderId::Agy,
            "AGY",
            "Antigravity",
            Some(0),
            Some("16:34".into()),
            Some(73),
            Some("137h".into()),
        );
        assert_eq!(minimized_reset(&session_depleted), Some(("16:34", true)));

        let healthy = ProviderQuota::new(
            ProviderId::Glm,
            "GLM",
            "Z.ai",
            Some(90),
            Some("16:41".into()),
            Some(85),
            Some("01.10.".into()),
        );
        assert_eq!(minimized_reset(&healthy), None);
    }

    #[test]
    fn minimized_codex_with_rounded_zero_week_without_confirmed_lock_hides_week_reset() {
        let mut codex = ProviderQuota::new(
            ProviderId::Codex,
            "Codex",
            "OpenAI",
            Some(40),
            Some("16:34".into()),
            Some(0),
            Some("25.11. (12:00)".into()),
        );
        codex.week_confirmed_exhausted = false;
        codex.five_hour_confirmed_exhausted = false;
        codex.explicit_provider_throttle = true;
        codex.is_throttled = true;
        assert_eq!(minimized_reset(&codex), None);

        codex.explicit_provider_throttle = false;
        codex.five_hour_confirmed_exhausted = true;
        assert_eq!(minimized_reset(&codex), Some(("16:34", true)));
    }

    fn agy_account_card(account: &str) -> ProviderQuota {
        let mut card = ProviderQuota::new(ProviderId::Agy, "AGY", "Antigravity", Some(40), None, Some(70), None);
        card.account_id = Some(account.to_string());
        card
    }

    #[test]
    fn drop_between_two_account_cards_snaps_behind_the_whole_group() {
        let visible_without = vec![ProviderId::Glm, ProviderId::Agy, ProviderId::Agy, ProviderId::Codex];
        let mut full = vec![ProviderId::Claude, ProviderId::Glm, ProviderId::Agy, ProviderId::Codex];
        // Claude dropped between the two AGY cards.
        reorder_quota_order(&mut full, ProviderId::Claude, &visible_without, 2);
        assert_eq!(
            full,
            vec![ProviderId::Glm, ProviderId::Agy, ProviderId::Claude, ProviderId::Codex]
        );
    }

    #[test]
    fn drop_in_front_of_an_account_group_lands_exactly_before_it() {
        let visible_without = vec![ProviderId::Claude, ProviderId::Glm, ProviderId::Agy, ProviderId::Agy];
        let mut full = vec![ProviderId::Claude, ProviderId::Glm, ProviderId::Agy, ProviderId::Codex];
        // Codex dropped in front of the first AGY card.
        reorder_quota_order(&mut full, ProviderId::Codex, &visible_without, 2);
        assert_eq!(
            full,
            vec![ProviderId::Claude, ProviderId::Glm, ProviderId::Codex, ProviderId::Agy]
        );
    }

    #[test]
    fn drop_past_the_last_account_card_appends_after_the_group() {
        let visible_without = vec![ProviderId::Claude, ProviderId::Glm, ProviderId::Agy, ProviderId::Agy];
        let mut full = vec![ProviderId::Codex, ProviderId::Claude, ProviderId::Glm, ProviderId::Agy];
        reorder_quota_order(&mut full, ProviderId::Codex, &visible_without, visible_without.len());
        assert_eq!(
            full,
            vec![ProviderId::Claude, ProviderId::Glm, ProviderId::Agy, ProviderId::Codex]
        );
    }

    #[test]
    fn account_cards_keep_their_slot_order_and_distinct_widget_ids() {
        let mut providers = vec![agy_account_card("acc1"), agy_account_card("acc2")];
        apply_quota_order(&mut providers, &[ProviderId::Agy]);
        assert_eq!(
            providers.iter().map(|quota| quota.account_id.clone()).collect::<Vec<_>>(),
            vec![Some("acc1".to_string()), Some("acc2".to_string())]
        );
        assert_ne!(
            card_widget_id(ProviderId::Agy, Some("acc1")),
            card_widget_id(ProviderId::Agy, Some("acc2"))
        );
        assert_ne!(
            card_widget_id(ProviderId::Agy, Some("acc1")),
            card_widget_id(ProviderId::Agy, None)
        );
    }

    #[test]
    fn reset_text_keeps_the_full_form_when_it_fits() {
        let full = "Reset in ~4d 13h (25.11. 00:29)";
        assert_eq!(fit_reset_text(full, 400.0, |text| text.len() as f32 * 6.0), full);
    }

    #[test]
    fn reset_text_drops_the_date_suffix_when_the_space_is_short() {
        let full = "Reset in ~4d 13h (25.11. 00:29)";
        assert_eq!(
            fit_reset_text(full, 120.0, |text| text.len() as f32 * 6.0),
            "Reset in ~4d 13h"
        );
    }

    #[test]
    fn reset_text_without_a_date_suffix_stays_whole_for_the_clip() {
        assert_eq!(
            fit_reset_text("Reset in 3h", 10.0, |text| text.len() as f32 * 6.0),
            "Reset in 3h"
        );
    }

    #[test]
    fn provider_slots_are_unique() {
        let slots = [
            provider_slot(ProviderId::Glm),
            provider_slot(ProviderId::Agy),
            provider_slot(ProviderId::Codex),
            provider_slot(ProviderId::Claude),
            provider_slot(ProviderId::Qwen),
        ];
        for (index, slot) in slots.iter().enumerate() {
            assert_eq!(*slot, index as u8);
        }
    }
}
