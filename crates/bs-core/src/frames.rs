//! Тайминги кадров: из потока present-таймстампов считаются FPS, frametime и низкие перцентили.
//!
//! Источник таймстампов платформенный (ETW на Windows, Vulkan-слой на Linux), но арифметика
//! общая и живёт здесь — она же единственная часть проекта, которую можно нормально покрыть
//! юнит-тестами.

use std::collections::VecDeque;

/// Сколько кадров держим в кольцевом буфере.
///
/// 0.1% low требует хотя бы тысячи кадров, чтобы вообще что-то значить, поэтому запас
/// выбран с расчётом на несколько секунд при высоком FPS.
const DEFAULT_CAPACITY: usize = 4096;

/// Если игра не презентила дольше этого времени — считаем, что кадров нет, и не показываем
/// «замороженный» FPS от предыдущей сцены.
const STALE_AFTER_NS: u64 = 1_000_000_000;

/// Минимум кадров, ниже которого соответствующий перцентиль не имеет смысла и не считается.
const MIN_FRAMES_FOR_1PCT: usize = 100;
const MIN_FRAMES_FOR_01PCT: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameMetrics {
    /// Мгновенный FPS по последнему интервалу между кадрами.
    pub fps: f32,
    /// Длительность последнего кадра в миллисекундах.
    pub frametime_ms: f32,
    /// Средний FPS по всему содержимому буфера.
    pub avg_fps: f32,
    /// FPS, соответствующий 99-му перцентилю frametime. `None`, если кадров слишком мало.
    pub low_1pct: Option<f32>,
    /// FPS, соответствующий 99.9-му перцентилю frametime. `None`, если кадров слишком мало.
    pub low_01pct: Option<f32>,
    /// Сколько кадров участвовало в расчёте.
    pub sample_count: usize,
}

/// Кольцевой буфер present-таймстампов в наносекундах.
#[derive(Debug)]
pub struct FrameTimeline {
    times: VecDeque<u64>,
    capacity: usize,
}

impl Default for FrameTimeline {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }
}

impl FrameTimeline {
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(
            capacity >= 2,
            "буфер кадров бессмыслен меньше двух элементов"
        );
        Self {
            times: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Добавляет таймстамп кадра.
    ///
    /// Не-возрастающие значения отбрасываются: ETW доставляет события пачками и порядок
    /// внутри пачки не гарантирован, а отрицательный frametime сломал бы всю статистику.
    pub fn push(&mut self, timestamp_ns: u64) {
        if let Some(&last) = self.times.back()
            && timestamp_ns <= last
        {
            return;
        }
        if self.times.len() == self.capacity {
            self.times.pop_front();
        }
        self.times.push_back(timestamp_ns);
    }

    /// Забывает всю историю — например, при смене игры в фокусе.
    pub fn clear(&mut self) {
        self.times.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.times.is_empty()
    }

    /// Считает метрики на момент `now_ns`.
    ///
    /// Возвращает `None`, если кадров меньше двух или последний кадр слишком старый —
    /// лучше не показать ничего, чем показать FPS игры, которая уже свернута.
    pub fn metrics(&self, now_ns: u64) -> Option<FrameMetrics> {
        if self.times.len() < 2 {
            return None;
        }
        let last = *self.times.back()?;
        if now_ns.saturating_sub(last) > STALE_AFTER_NS {
            return None;
        }

        let first = *self.times.front()?;
        let span_ns = last.saturating_sub(first);
        if span_ns == 0 {
            return None;
        }

        let intervals = self.times.len() - 1;
        let avg_fps = intervals as f64 * 1e9 / span_ns as f64;

        let mut frametimes_ms: Vec<f32> = self
            .times
            .iter()
            .zip(self.times.iter().skip(1))
            .map(|(a, b)| (b - a) as f32 / 1e6)
            .collect();

        let frametime_ms = *frametimes_ms.last()?;

        // Перцентили считаются по frametime, а не по FPS: «1% low» — это медленнейший
        // процент кадров, и усреднять обратные величины было бы неверно.
        frametimes_ms.sort_unstable_by(f32::total_cmp);
        let low_1pct = percentile_fps(&frametimes_ms, 0.99, MIN_FRAMES_FOR_1PCT);
        let low_01pct = percentile_fps(&frametimes_ms, 0.999, MIN_FRAMES_FOR_01PCT);

        Some(FrameMetrics {
            fps: 1000.0 / frametime_ms,
            frametime_ms,
            avg_fps: avg_fps as f32,
            low_1pct,
            low_01pct,
            sample_count: self.times.len(),
        })
    }
}

/// Берёт `p`-й перцентиль из отсортированных по возрастанию frametime и переводит в FPS.
///
/// `min_samples` защищает от бессмысленных цифр: 0.1% low на трёх кадрах — это просто
/// худший кадр, и показывать его под таким именем нечестно.
fn percentile_fps(sorted_frametimes_ms: &[f32], p: f32, min_samples: usize) -> Option<f32> {
    if sorted_frametimes_ms.len() < min_samples {
        return None;
    }
    let idx = ((sorted_frametimes_ms.len() - 1) as f32 * p).floor() as usize;
    let ft = sorted_frametimes_ms[idx];
    (ft > 0.0).then(|| 1000.0 / ft)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    /// Ровный поток кадров с заданным интервалом.
    fn steady(count: usize, interval_ms: u64) -> FrameTimeline {
        let mut t = FrameTimeline::default();
        for i in 0..count {
            t.push(i as u64 * interval_ms * MS);
        }
        t
    }

    #[test]
    fn empty_and_single_frame_yield_nothing() {
        let empty = FrameTimeline::default();
        assert!(empty.metrics(0).is_none());

        let mut one = FrameTimeline::default();
        one.push(1000 * MS);
        assert!(
            one.metrics(1000 * MS).is_none(),
            "по одному кадру frametime не определён"
        );
    }

    #[test]
    fn steady_stream_reports_matching_fps() {
        // 200 кадров по 10 мс = ровно 100 FPS.
        let t = steady(200, 10);
        let m = t.metrics(199 * 10 * MS).unwrap();

        assert!((m.fps - 100.0).abs() < 0.01);
        assert!((m.avg_fps - 100.0).abs() < 0.01);
        assert!((m.frametime_ms - 10.0).abs() < 0.001);
        assert_eq!(m.sample_count, 200);
    }

    #[test]
    fn lows_require_enough_samples_to_mean_anything() {
        let few = steady(50, 10).metrics(49 * 10 * MS).unwrap();
        assert!(
            few.low_1pct.is_none(),
            "1% low на 50 кадрах — это просто худший кадр"
        );
        assert!(few.low_01pct.is_none());

        let some = steady(200, 10).metrics(199 * 10 * MS).unwrap();
        assert!(some.low_1pct.is_some());
        assert!(
            some.low_01pct.is_none(),
            "0.1% low требует не меньше 1000 кадров"
        );

        let many = steady(1500, 10).metrics(1499 * 10 * MS).unwrap();
        assert!(many.low_01pct.is_some());
    }

    /// Поток из `count` кадров по 10 мс, где каждый `stutter_every`-й длится 100 мс.
    fn with_stutters(count: usize, stutter_every: usize) -> (FrameTimeline, u64) {
        let mut t = FrameTimeline::with_capacity(count + 1);
        let mut now = 0u64;
        for i in 0..count {
            now += if i > 0 && i % stutter_every == 0 {
                100 * MS
            } else {
                10 * MS
            };
            t.push(now);
        }
        (t, now)
    }

    #[test]
    fn stutters_drag_down_the_1pct_low_but_barely_touch_the_average() {
        // 2% кадров тормозят — это уверенно выше порога в 1%.
        let (t, now) = with_stutters(1000, 50);
        let m = t.metrics(now).unwrap();

        assert!(
            m.avg_fps > 70.0,
            "средний FPS почти не страдает: {}",
            m.avg_fps
        );
        let low = m.low_1pct.unwrap();
        assert!(
            low < 20.0,
            "1% low обязан показать стомиллисекундные кадры, получено {low}"
        );
    }

    /// Это не баг, а определение метрики, и его стоит зафиксировать тестом.
    ///
    /// «1% low» — это 99-й перцентиль frametime. Один фриз на двести кадров составляет
    /// полпроцента выборки, то есть не дотягивает до порога, и перцентиль его не показывает.
    /// Чтобы такие одиночные события были видны, в оверлее нужен график frametime — он и
    /// запланирован отдельно.
    #[test]
    fn a_lone_stutter_below_the_one_percent_threshold_does_not_move_the_1pct_low() {
        let mut t = FrameTimeline::default();
        let mut now = 0u64;
        for i in 0..200 {
            now += if i == 100 { 100 * MS } else { 10 * MS };
            t.push(now);
        }
        let m = t.metrics(now).unwrap();

        let low = m.low_1pct.unwrap();
        assert!(
            (low - 100.0).abs() < 1.0,
            "99% кадров по-прежнему укладываются в 10 мс: {low}"
        );
    }

    #[test]
    fn out_of_order_timestamps_are_dropped_not_treated_as_negative_frametime() {
        let mut t = FrameTimeline::default();
        t.push(100 * MS);
        t.push(110 * MS);
        t.push(105 * MS); // пришёл с опозданием — игнорируем
        t.push(120 * MS);

        let m = t.metrics(120 * MS).unwrap();
        assert_eq!(m.sample_count, 3);
        assert!(m.frametime_ms > 0.0);
    }

    #[test]
    fn duplicate_timestamps_are_dropped() {
        let mut t = FrameTimeline::default();
        t.push(100 * MS);
        t.push(100 * MS);
        assert!(
            t.metrics(100 * MS).is_none(),
            "два одинаковых таймстампа — это один кадр"
        );
    }

    #[test]
    fn stale_frames_report_nothing_rather_than_a_frozen_number() {
        let t = steady(200, 10);
        let last = 199 * 10 * MS;
        assert!(
            t.metrics(last + 500 * MS).is_some(),
            "полсекунды — ещё живо"
        );
        assert!(
            t.metrics(last + 3_000 * MS).is_none(),
            "через три секунды без кадров показывать старый FPS нельзя"
        );
    }

    #[test]
    fn ring_buffer_evicts_oldest_and_keeps_reporting() {
        let mut t = FrameTimeline::with_capacity(8);
        for i in 0..100 {
            t.push(i as u64 * 10 * MS);
        }
        let m = t.metrics(99 * 10 * MS).unwrap();
        assert_eq!(m.sample_count, 8);
        assert!((m.avg_fps - 100.0).abs() < 0.01);
    }

    #[test]
    fn clear_forgets_everything() {
        let mut t = steady(200, 10);
        t.clear();
        assert!(t.is_empty());
        assert!(t.metrics(0).is_none());
    }
}
