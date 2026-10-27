//! Pure ordering logic for the quota cards, kept platform-neutral so its
//! regression tests run on every development machine.
use crate::quota::{ProviderId, ProviderQuota};

/// Stable per-provider slot for egui widget ids (ProviderId is not Hash).
pub(crate) fn provider_slot(provider: ProviderId) -> u8 {
    match provider {
        ProviderId::Glm => 0,
        ProviderId::Agy => 1,
        ProviderId::Codex => 2,
        ProviderId::Claude => 3,
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

/// Move `provider` so it lands at `visible_index` among `visible`, which must
/// already exclude the dragged provider itself (the callers pass the list
/// without it). The target anchor is resolved only after the dragged provider
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
        order
            .iter()
            .position(|entry| *entry == visible[visible_index])
            .unwrap_or(order.len())
    };
    let insert_at = order_index.min(order.len());
    order.insert(insert_at, provider);
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
    fn provider_slots_are_unique() {
        let slots = [
            provider_slot(ProviderId::Glm),
            provider_slot(ProviderId::Agy),
            provider_slot(ProviderId::Codex),
            provider_slot(ProviderId::Claude),
        ];
        for (index, slot) in slots.iter().enumerate() {
            assert_eq!(*slot, index as u8);
        }
    }
}
