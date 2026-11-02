use std::collections::BTreeSet;
use std::time::Duration;

use super::*;

pub(super) enum Step {
    Batch(Vec<InputEvent>),
    Delay(Duration),
    CursorInside,
}

pub(super) fn plan(ticket: &WindowTicket, action: &NativeAction) -> Result<Vec<Step>, String> {
    let point = |x, y| {
        if !ticket.client.contains(x, y) {
            return Err("point outside bound client".into());
        }
        ticket
            .desktop
            .normalize(x, y)
            .map(|(x, y)| InputEvent::MoveAbsolute { x, y })
            .map_err(|e| e.to_string())
    };
    let button = |button, down| InputEvent::Button { button, down };
    let steps = match action {
        NativeAction::MoveTo { x, y } => vec![Step::Batch(vec![point(*x, *y)?])],
        NativeAction::ClickAt { x, y, button: key } => vec![Step::Batch(vec![
            point(*x, *y)?,
            button(*key, true),
            button(*key, false),
        ])],
        NativeAction::KeyChord { keys } => {
            if keys.is_empty()
                || keys.len() > 8
                || keys
                    .iter()
                    .map(|key| key.family())
                    .collect::<BTreeSet<_>>()
                    .len()
                    != keys.len()
            {
                return Err("key chord requires 1..=8 distinct keys".into());
            }
            let mut events: Vec<_> = keys
                .iter()
                .map(|&key| InputEvent::Key { key, down: true })
                .collect();
            events.extend(
                keys.iter()
                    .rev()
                    .map(|&key| InputEvent::Key { key, down: false }),
            );
            vec![Step::Batch(events)]
        }
        NativeAction::TypeText { text } => {
            if text.is_empty() || text.chars().count() > 4096 || text.contains('\0') {
                return Err("text requires 1..=4096 characters without NUL".into());
            }
            text.encode_utf16()
                .collect::<Vec<_>>()
                .chunks(32)
                .map(|units| {
                    Step::Batch(
                        units
                            .iter()
                            .flat_map(|&unit| {
                                [
                                    InputEvent::Unicode { unit, down: true },
                                    InputEvent::Unicode { unit, down: false },
                                ]
                            })
                            .collect(),
                    )
                })
                .collect()
        }
        NativeAction::Drag {
            from,
            to,
            button: key,
            steps,
            duration_ms,
        } => {
            if !(1..=64).contains(steps) || *duration_ms > 5000 {
                return Err("drag requires 1..=64 steps and at most 5000 ms".into());
            }
            let first = point(from.0, from.1)?;
            let _last = point(to.0, to.1)?;
            let mut result = vec![Step::Batch(vec![first, button(*key, true)])];
            for index in 1..=*steps {
                let interpolate = |start, end| {
                    (f64::from(start)
                        + (f64::from(end) - f64::from(start)) * f64::from(index)
                            / f64::from(*steps))
                    .round() as i32
                };
                let x = interpolate(from.0, to.0);
                let y = interpolate(from.1, to.1);
                let before = u64::from(*duration_ms) * u64::from(index - 1) / u64::from(*steps);
                let after = u64::from(*duration_ms) * u64::from(index) / u64::from(*steps);
                result.push(Step::Delay(Duration::from_millis(after - before)));
                let mut events = vec![point(x, y)?];
                if index == *steps {
                    events.push(button(*key, false));
                }
                result.push(Step::Batch(events));
            }
            result
        }
        NativeAction::Scroll { horizontal, delta } => {
            if *delta == 0 || delta.unsigned_abs() > 120 * 100 {
                return Err("scroll delta must be nonzero and at most 100 wheel notches".into());
            }
            vec![
                Step::CursorInside,
                Step::Batch(vec![InputEvent::Scroll {
                    horizontal: *horizontal,
                    delta: *delta,
                }]),
            ]
        }
        NativeAction::Wait { milliseconds } => {
            if *milliseconds > 5000 {
                return Err("wait cannot exceed 5000 ms".into());
            }
            vec![Step::Delay(Duration::from_millis(u64::from(*milliseconds)))]
        }
        NativeAction::Noop => Vec::new(),
    };
    Ok(steps)
}
