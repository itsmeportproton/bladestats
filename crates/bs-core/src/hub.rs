//! Обмен снапшотом между потоками.
//!
//! Потоков три: сэмплер телеметрии (медленный, ~500 мс), источник кадров (быстрый, колбэк ETW
//! или Vulkan-слоя) и рендер (10–20 Гц). Рендер не должен ждать никого, поэтому обмен идёт
//! через [`arc_swap`], а не через мьютекс: читатель забирает указатель, писатель публикует
//! новый снапшот целиком.

use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::snapshot::MetricsSnapshot;

/// Точка обмена снапшотом. Клонируется в каждый поток.
#[derive(Clone)]
pub struct SnapshotHub {
    inner: Arc<ArcSwap<MetricsSnapshot>>,
}

impl Default for SnapshotHub {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotHub {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ArcSwap::from_pointee(MetricsSnapshot::default())),
        }
    }

    /// Забирает текущий снапшот. Не блокируется и не ждёт писателя.
    pub fn load(&self) -> Arc<MetricsSnapshot> {
        self.inner.load_full()
    }

    /// Публикует новый снапшот, заменяя предыдущий целиком.
    pub fn store(&self, snapshot: MetricsSnapshot) {
        self.inner.store(Arc::new(snapshot));
    }

    /// Правит текущий снапшот и публикует результат.
    ///
    /// Обновления от разных источников независимы (телеметрия отдельно, кадры отдельно),
    /// поэтому здесь возможна гонка «потерянное обновление»: два писателя, прочитавшие один
    /// и тот же снапшот, затрут правки друг друга. Для оверлея это приемлемо — цена промаха
    /// в один тик отрисовки, а не испорченные данные.
    pub fn update(&self, f: impl FnOnce(&mut MetricsSnapshot)) {
        let mut next = (*self.load()).clone();
        f(&mut next);
        self.store(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Power;

    #[test]
    fn starts_empty_so_the_ui_shows_nothing_before_the_first_sample() {
        let hub = SnapshotHub::new();
        let s = hub.load();
        assert!(s.cpu.name.is_none());
        assert!(s.cpu.cores.is_empty());
        assert!(s.frames.is_none());
    }

    #[test]
    fn store_replaces_and_readers_see_it() {
        let hub = SnapshotHub::new();
        let mut s = MetricsSnapshot::default();
        s.cpu.name = Some("AMD Ryzen 7 7800X3D".into());
        hub.store(s);

        assert_eq!(hub.load().cpu.name.as_deref(), Some("AMD Ryzen 7 7800X3D"));
    }

    #[test]
    fn update_edits_in_place_without_losing_other_fields() {
        let hub = SnapshotHub::new();
        hub.update(|s| s.cpu.name = Some("Intel Core i9-13900K".into()));
        hub.update(|s| s.cpu.power = Some(Power::Estimated(125.0)));

        let s = hub.load();
        assert_eq!(s.cpu.name.as_deref(), Some("Intel Core i9-13900K"));
        assert_eq!(s.cpu.power, Some(Power::Estimated(125.0)));
    }

    #[test]
    fn clones_share_one_slot() {
        let writer = SnapshotHub::new();
        let reader = writer.clone();
        writer.update(|s| s.memory.speed_mhz = Some(6000));

        assert_eq!(reader.load().memory.speed_mhz, Some(6000));
    }

    #[test]
    fn a_reader_holding_a_snapshot_is_unaffected_by_later_writes() {
        let hub = SnapshotHub::new();
        hub.update(|s| s.memory.speed_mhz = Some(3200));
        let held = hub.load();

        hub.update(|s| s.memory.speed_mhz = Some(6000));

        assert_eq!(
            held.memory.speed_mhz,
            Some(3200),
            "снапшот у читателя неизменяем"
        );
        assert_eq!(hub.load().memory.speed_mhz, Some(6000));
    }
}
