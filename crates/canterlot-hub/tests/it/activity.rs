use std::time::Duration;

use canterlot_hub::{
    activity::{
        self, ActivityInputs, ActivityState as S, Evidence as E, ExecutionFacts, LinkState,
        ProcessFacts, QueueFacts, Thresholds,
    },
    model::{CompactionProgress, ExecutionState, HoldReason, PauseReason, RunSlot},
};
use chrono::{TimeDelta, Utc};
use uuid::Uuid;

fn thresholds() -> Thresholds {
    Thresholds {
        stalled_generating: Duration::from_secs(1800),
        stalled_tool: Duration::from_secs(1800),
        hung_generating: Duration::from_secs(600),
        hung_rescue: canterlot_hub::config::HungRescue::Auto,
        idle_quiet: Duration::from_secs(5),
        idle_background_max: std::time::Duration::from_secs(15 * 60),
    }
}

fn inputs() -> ActivityInputs {
    let now = Utc::now();
    ActivityInputs {
        now,
        execution: Some(ExecutionFacts {
            state: ExecutionState::Running,
            outcome: None,
            phase: "generating".into(),
            phase_at: Some(now),
            tool_name: None,
            summary: None,
        }),
        queue: QueueFacts {
            slot: RunSlot::Running {
                input_id: Uuid::now_v7(),
            },
            queued: 0,
            pause_reasons: vec![],
            hold_reasons: vec![],
        },
        open_requests: 0,
        open_secret_requests: 0,
        secret_question: None,
        last_event_at: Some(now),
        last_turn_at: None,
        turn_started_at: Some(now),
        process: Some(ProcessFacts {
            alive: true,
            cpu_permille: 50,
            descendants: Some(0),
        }),
        link: LinkState::Available,
        awaiting_attachments: false,
        background_work: None,
        wake_turn: false,
        question_summary: None,
        lease_wait: None,
    }
}

#[test]
fn activity_idle_carries_background_work_count() {
    let mut i = inputs();
    i.queue.slot = RunSlot::Free;
    i.background_work = Some(2);
    let activity = activity::compute(&i, &thresholds());
    assert_eq!(activity.state, S::Idle);
    assert!(matches!(
        activity.evidence,
        E::Idle {
            background_work: Some(2),
            ..
        }
    ));
}

#[test]
fn wake_turn_without_a_slot_is_working() {
    let mut i = inputs();
    i.queue.slot = RunSlot::Free;
    i.wake_turn = true;
    let activity = activity::compute(&i, &thresholds());
    assert_eq!(activity.state, S::Working);
    assert!(matches!(activity.evidence, E::Working { ref phase, .. } if phase == "generating"));
}

#[test]
fn wake_turn_of_a_stopped_execution_is_not_working() {
    let mut i = inputs();
    i.queue.slot = RunSlot::Free;
    i.wake_turn = true;
    i.execution.as_mut().unwrap().state = ExecutionState::Stopped;
    assert_eq!(activity::compute(&i, &thresholds()).state, S::Stopped);
}

#[test]
fn state_table_matches_pilot_section_9() {
    let base = inputs();
    let mut rows = Vec::new();
    let mut i = base.clone();
    i.queue.slot = RunSlot::Free;
    rows.push((
        i,
        S::Idle,
        E::Idle {
            last_turn_at: None,
            background_work: None,
            process_alive: Some(true),
        },
    ));
    rows.push((
        base.clone(),
        S::Working,
        E::Working {
            phase: "generating".into(),
            tool_name: None,
            summary: None,
            phase_since: Some(base.now),
            lease_wait: None,
        },
    ));
    let mut i = base.clone();
    i.execution.as_mut().unwrap().phase = "awaiting_answer".into();
    rows.push((
        i,
        S::WaitingForUser,
        E::WaitingForUser {
            phase: "awaiting_answer".into(),
            question: None,
        },
    ));
    let mut i = base.clone();
    i.now += TimeDelta::seconds(1801);
    i.process.as_mut().unwrap().cpu_permille = 0;
    rows.push((
        i,
        S::Stalled,
        E::Stalled {
            last_event_at: base.now,
            phase: "generating".into(),
            process_alive: Some(true),
            cpu_busy: Some(false),
            link: "available".into(),
            hung: Some(true),
        },
    ));
    let mut i = base.clone();
    i.queue.slot = RunSlot::Free;
    i.queue.hold_reasons.push(HoldReason::QuotaExhausted {
        provider: "claude".into(),
        window: "five_hour".into(),
        wake_at: base.now,
    });
    rows.push((
        i,
        S::QuotaPaused,
        E::QuotaPaused {
            reason: "quota_exhausted".into(),
            provider: Some("claude".into()),
            window: Some("five_hour".into()),
            until: Some(base.now),
        },
    ));
    let mut i = base.clone();
    i.execution.as_mut().unwrap().state = ExecutionState::Failed;
    i.execution.as_mut().unwrap().outcome = Some("fatal_error".into());
    rows.push((
        i,
        S::Failed,
        E::Failed {
            reason: "fatal_error".into(),
        },
    ));
    let mut i = base;
    i.execution.as_mut().unwrap().state = ExecutionState::Stopped;
    i.execution.as_mut().unwrap().outcome = Some("user_stop".into());
    rows.push((
        i,
        S::Stopped,
        E::Stopped {
            outcome: "user_stop".into(),
        },
    ));
    for (i, state, evidence) in rows {
        let actual = activity::compute(&i, &thresholds());
        assert_eq!(actual.state, state);
        assert_eq!(actual.evidence, evidence);
    }
}

#[test]
fn running_turn_beats_quota_hold() {
    let mut i = inputs();
    i.queue.hold_reasons.push(HoldReason::WeeklyReserve {
        provider: "codex".into(),
        resets_at: None,
    });
    assert_eq!(activity::compute(&i, &thresholds()).state, S::Working);
    i.queue.slot = RunSlot::Free;
    assert!(
        matches!(activity::compute(&i, &thresholds()).evidence, E::QuotaPaused { reason, .. } if reason == "weekly_reserve")
    );
    i.queue.hold_reasons.clear();
    i.queue.pause_reasons.push(PauseReason::LimitReached {
        provider: None,
        window: None,
        resets_at: None,
    });
    assert!(
        matches!(activity::compute(&i, &thresholds()).evidence, E::QuotaPaused { reason, .. } if reason == "limit_reached")
    );
}

#[test]
fn a_new_turn_after_a_long_idle_counts_silence_from_its_own_start() {
    let mut i = inputs();
    i.process.as_mut().unwrap().cpu_permille = 0;
    i.last_event_at = Some(i.now - TimeDelta::hours(1));
    i.execution.as_mut().unwrap().phase_at = Some(i.now - TimeDelta::hours(1));
    i.turn_started_at = Some(i.now);
    assert_eq!(activity::compute(&i, &thresholds()).state, S::Working);

    i.now += TimeDelta::seconds(1801);
    let activity = activity::compute(&i, &thresholds());
    assert_eq!(activity.state, S::Stalled);
    assert_eq!(
        activity.evidence,
        E::Stalled {
            last_event_at: i.now - TimeDelta::seconds(1801),
            phase: "generating".into(),
            process_alive: Some(true),
            cpu_busy: Some(false),
            link: "available".into(),
            hung: Some(true),
        },
        "the stall clock starts at the turn, not at the previous event"
    );
}

#[test]
fn a_new_turn_in_the_same_phase_starts_the_phase_clock_again() {
    // Прошлый ход закончился в generating: исполнение так и стоит в этой фазе,
    // и её отметка относится к прошлому ходу.
    let mut i = inputs();
    i.turn_started_at = Some(i.now);
    i.execution.as_mut().unwrap().phase = "generating".into();
    i.execution.as_mut().unwrap().phase_at = Some(i.now - TimeDelta::hours(6));
    let activity = activity::compute(&i, &thresholds());
    assert_eq!(activity.state, S::Working);
    assert!(
        matches!(
            activity.evidence,
            E::Working {
                phase_since: Some(at),
                ..
            } if at == i.now
        ),
        "phase_since must not be earlier than the current turn: {:?}",
        activity.evidence
    );

    // Отметка фазы вообще без времени: опорой остаётся начало хода.
    i.execution.as_mut().unwrap().phase_at = None;
    assert!(matches!(
        activity::compute(&i, &thresholds()).evidence,
        E::Working {
            phase_since: Some(at),
            ..
        } if at == i.now
    ));

    // Фаза, начавшаяся внутри хода, остаётся точнее его начала.
    let phase_at = i.now + TimeDelta::minutes(2);
    i.now = phase_at + TimeDelta::seconds(5);
    i.execution.as_mut().unwrap().phase_at = Some(phase_at);
    assert!(matches!(
        activity::compute(&i, &thresholds()).evidence,
        E::Working {
            phase_since: Some(at),
            ..
        } if at == phase_at
    ));
}

#[test]
fn stalled_thresholds_are_generous_for_generation_and_configurable() {
    for (phase, seconds) in [
        ("generating", 1800),
        ("unknown", 1800),
        ("running_tool", 1800),
        ("context_maintenance", 1800),
    ] {
        let mut i = inputs();
        i.process.as_mut().unwrap().cpu_permille = 0;
        // Живой потомок исключает verdict «завис в генерации»: здесь проверяется
        // именно старый щедрый порог `stalled`, а не новый быстрый порог hang.
        // У инструментов живое дерево и есть доказательство работы, его не трогаем.
        if !matches!(phase, "running_tool" | "context_maintenance") {
            i.process.as_mut().unwrap().descendants = Some(1);
        }
        i.execution.as_mut().unwrap().phase = phase.into();
        i.now += TimeDelta::seconds(seconds);
        assert_eq!(activity::compute(&i, &thresholds()).state, S::Working);
        i.now += TimeDelta::seconds(1);
        assert_eq!(activity::compute(&i, &thresholds()).state, S::Stalled);
        i.execution.as_mut().unwrap().phase_at = Some(i.now);
        assert_eq!(activity::compute(&i, &thresholds()).state, S::Working);
        i.execution.as_mut().unwrap().phase_at = i.last_event_at;
        let custom = Thresholds {
            stalled_generating: Duration::from_secs(4000),
            stalled_tool: Duration::from_secs(4000),
            hung_generating: Duration::from_secs(600),
            hung_rescue: canterlot_hub::config::HungRescue::Auto,
            idle_quiet: Duration::from_secs(5),
            idle_background_max: std::time::Duration::from_secs(15 * 60),
        };
        assert_eq!(activity::compute(&i, &custom).state, S::Working);
    }
}

#[test]
fn waiting_for_user_is_never_stalled() {
    for (phase, requests) in [
        ("awaiting_permission", 0),
        ("awaiting_answer", 0),
        ("generating", 1),
    ] {
        let mut i = inputs();
        i.now += TimeDelta::days(1);
        i.open_requests = requests;
        i.execution.as_mut().unwrap().phase = phase.into();
        assert_eq!(
            activity::compute(&i, &thresholds()).state,
            S::WaitingForUser
        );
    }
}

#[test]
fn owner_lost_is_failed_with_reason() {
    let mut i = inputs();
    i.execution.as_mut().unwrap().state = ExecutionState::OwnerLost;
    assert_eq!(
        activity::compute(&i, &thresholds()).evidence,
        E::Failed {
            reason: "owner_lost".into()
        }
    );
}

#[test]
fn stuck_session_is_failed_with_that_reason_while_the_execution_lives() {
    let mut i = inputs();
    i.queue.slot = RunSlot::Free;
    i.queue
        .pause_reasons
        .push(PauseReason::SessionStuck { compaction: None });
    assert_eq!(
        activity::compute(&i, &thresholds()).evidence,
        E::Failed {
            reason: "session_stuck".into()
        }
    );
    i.queue.pause_reasons = vec![PauseReason::SessionStuck {
        compaction: Some(CompactionProgress::Requested),
    }];
    let activity = activity::compute(&i, &thresholds());
    assert!(matches!(
        activity.evidence,
        E::Working { ref phase, .. } if phase == "context_maintenance"
    ));
}

#[test]
fn context_exhausted_is_failed_with_that_reason() {
    let mut i = inputs();
    i.execution.as_mut().unwrap().state = ExecutionState::Failed;
    i.execution.as_mut().unwrap().outcome = Some("context_exhausted".into());
    assert_eq!(
        activity::compute(&i, &thresholds()).evidence,
        E::Failed {
            reason: "context_exhausted".into()
        }
    );
}

#[test]
fn starting_and_stopping_map_to_working_with_phase() {
    for (state, phase) in [
        (ExecutionState::Created, "starting"),
        (ExecutionState::Starting, "starting"),
        (ExecutionState::Stopping, "stopping"),
    ] {
        let mut i = inputs();
        i.execution.as_mut().unwrap().state = state;
        i.queue.slot = RunSlot::Free;
        i.now += TimeDelta::days(1);
        let a = activity::compute(&i, &thresholds());
        assert_eq!(a.state, S::Working);
        assert!(matches!(a.evidence, E::Working { phase: p, .. } if p == phase));
    }
}

#[test]
fn never_started_conversation_is_idle() {
    let mut i = inputs();
    i.execution = None;
    assert_eq!(activity::compute(&i, &thresholds()).state, S::Idle);
}

#[test]
fn link_loss_keeps_working_with_phase_no_link_instead_of_stalled() {
    for link in [LinkState::Degraded, LinkState::Unavailable] {
        let mut i = inputs();
        i.link = link;
        i.now += TimeDelta::days(1);
        assert!(
            matches!(activity::compute(&i, &thresholds()).evidence, E::Working { phase, .. } if phase == "no_link")
        );
    }
}

#[test]
fn attachment_wait_is_working_delivering_attachments_and_never_stalled() {
    let mut i = inputs();
    i.awaiting_attachments = true;
    i.now += TimeDelta::days(1);
    assert!(
        matches!(activity::compute(&i, &thresholds()).evidence, E::Working { phase, .. } if phase == "delivering_attachments")
    );
}

#[test]
fn running_tool_with_live_descendants_is_working_even_when_the_cli_is_idle() {
    let mut i = inputs();
    i.execution.as_mut().unwrap().phase = "running_tool".into();
    i.process = Some(ProcessFacts {
        alive: true,
        cpu_permille: 0,
        descendants: Some(1),
    });
    i.now += TimeDelta::hours(2);
    assert_eq!(activity::compute(&i, &thresholds()).state, S::Working);
}

#[test]
fn running_tool_with_no_live_descendants_is_stalled_after_the_tool_threshold() {
    let mut i = inputs();
    i.execution.as_mut().unwrap().phase = "running_tool".into();
    i.process = Some(ProcessFacts {
        alive: true,
        cpu_permille: 0,
        descendants: Some(0),
    });
    i.now += TimeDelta::seconds(1800);
    assert_eq!(activity::compute(&i, &thresholds()).state, S::Working);
    i.now += TimeDelta::seconds(1);
    assert_eq!(activity::compute(&i, &thresholds()).state, S::Stalled);
}

/// Те же сроки, что дают `stalled` молчащему инструменту без потомков, но
/// процесс ждёт очереди тяжёлых задач: это работа, и место видно в свидетельстве.
#[test]
fn waiting_for_a_lease_is_never_stalled() {
    use canterlot_hub::leases::{LeaseKind, LeaseWait};
    for phase in ["running_tool", "generating"] {
        let mut i = inputs();
        i.execution.as_mut().unwrap().phase = phase.into();
        i.process = Some(ProcessFacts {
            alive: true,
            cpu_permille: 0,
            descendants: Some(0),
        });
        i.now += TimeDelta::hours(5);
        assert_eq!(activity::compute(&i, &thresholds()).state, S::Stalled);
        let wait = LeaseWait {
            kind: LeaseKind::Build,
            position: 2,
            since: i.now - TimeDelta::hours(4),
        };
        i.lease_wait = Some(wait.clone());
        let a = activity::compute(&i, &thresholds());
        assert_eq!(a.state, S::Working, "{phase}");
        let evidence = serde_json::to_value(&a.evidence).unwrap();
        assert_eq!(evidence["lease_wait"]["kind"], "build");
        assert_eq!(evidence["lease_wait"]["position"], 2);
        let E::Working { lease_wait, .. } = a.evidence else {
            panic!("working evidence")
        };
        assert_eq!(lease_wait, Some(wait));
    }
    // Без ожидания поле не появляется вовсе: старые клиенты видят прежнюю форму.
    let evidence =
        serde_json::to_value(activity::compute(&inputs(), &thresholds()).evidence).unwrap();
    assert!(evidence.get("lease_wait").is_none());
}

#[test]
fn an_old_worker_without_descendant_evidence_does_not_stall_a_running_tool() {
    let mut i = inputs();
    i.execution.as_mut().unwrap().phase = "running_tool".into();
    i.process = Some(ProcessFacts {
        alive: true,
        cpu_permille: 0,
        descendants: None,
    });
    i.now += TimeDelta::hours(2);
    assert_eq!(activity::compute(&i, &thresholds()).state, S::Working);
}

#[test]
fn generating_with_busy_cpu_is_working() {
    let mut i = inputs();
    i.process = Some(ProcessFacts {
        alive: true,
        cpu_permille: 500,
        descendants: Some(0),
    });
    i.now += TimeDelta::hours(2);
    assert_eq!(activity::compute(&i, &thresholds()).state, S::Working);
}

#[test]
fn generating_without_progress_is_stalled_only_after_a_generous_threshold() {
    let generous = Thresholds {
        stalled_generating: Duration::from_secs(1800),
        stalled_tool: Duration::from_secs(1800),
        hung_generating: Duration::from_secs(600),
        hung_rescue: canterlot_hub::config::HungRescue::Auto,
        idle_quiet: Duration::from_secs(5),
        idle_background_max: std::time::Duration::from_secs(15 * 60),
    };
    let mut i = inputs();
    // Генерация: ищем исключительно старый порог `stalled`, поэтому дерево живо.
    i.process.as_mut().unwrap().cpu_permille = 0;
    i.process.as_mut().unwrap().descendants = Some(1);
    i.now += TimeDelta::seconds(1801);
    assert_eq!(activity::compute(&i, &generous).state, S::Stalled);
    assert!(matches!(
        activity::compute(&i, &generous).evidence,
        E::Stalled { hung: None, .. }
    ));
}

#[test]
fn unknown_liveness_never_claims_a_stall() {
    for phase in ["generating", "running_tool", "context_maintenance"] {
        let mut i = inputs();
        i.execution.as_mut().unwrap().phase = phase.into();
        i.process = None;
        i.now += TimeDelta::days(1);
        assert_eq!(
            activity::compute(&i, &thresholds()).state,
            S::Working,
            "{phase}"
        );
    }
}

#[test]
fn reserved_background_work_field_is_unknown_until_stage_2() {
    let mut i = inputs();
    i.queue.slot = RunSlot::Free;
    assert_eq!(
        activity::compute(&i, &thresholds()).evidence,
        E::Idle {
            last_turn_at: None,
            background_work: None,
            process_alive: Some(true),
        }
    );
    i.background_work = Some(2);
    assert!(matches!(
        activity::compute(&i, &thresholds()).evidence,
        E::Idle {
            background_work: Some(2),
            ..
        }
    ));
}

#[test]
fn idle_with_a_live_process_says_so() {
    let mut i = inputs();
    i.queue.slot = RunSlot::Free;
    let activity = activity::compute(&i, &thresholds());
    assert_eq!(activity.state, S::Idle);
    assert!(matches!(
        activity.evidence,
        E::Idle {
            process_alive: Some(true),
            ..
        }
    ));
}

#[test]
fn idle_whose_process_died_is_not_alive() {
    let mut i = inputs();
    i.queue.slot = RunSlot::Free;
    i.process = Some(ProcessFacts {
        alive: false,
        cpu_permille: 0,
        descendants: None,
    });
    assert!(matches!(
        activity::compute(&i, &thresholds()).evidence,
        E::Idle {
            process_alive: Some(false),
            ..
        }
    ));
}

#[test]
fn idle_without_an_execution_has_no_process() {
    let mut i = inputs();
    i.queue.slot = RunSlot::Free;
    i.execution = None;
    i.process = None;
    assert!(matches!(
        activity::compute(&i, &thresholds()).evidence,
        E::Idle {
            process_alive: Some(false),
            ..
        }
    ));
}

/// Без свежей телеметрии машины (старый воркер, пропавшие метрики) хаб не знает,
/// жив ли процесс: поле не пишется, и клиент показывает простой как раньше.
#[test]
fn idle_without_process_metrics_leaves_the_flag_unknown() {
    let mut i = inputs();
    i.queue.slot = RunSlot::Free;
    i.process = None;
    let activity = activity::compute(&i, &thresholds());
    assert!(matches!(
        activity.evidence,
        E::Idle {
            process_alive: None,
            ..
        }
    ));
    let json = serde_json::to_value(&activity.evidence).unwrap();
    assert!(json.get("process_alive").is_none(), "{json}");
}

fn silent_generating_turn() -> ActivityInputs {
    let mut i = inputs();
    i.process = Some(ProcessFacts {
        alive: true,
        cpu_permille: 0,
        descendants: Some(0),
    });
    // Порог `hung_generating` — 10 минут: 601 секунда тишины при живом, но пустом дереве.
    i.now += TimeDelta::seconds(601);
    i
}

#[test]
fn generating_silence_with_idle_cpu_and_no_descendants_is_hung() {
    let i = silent_generating_turn();
    let activity = activity::compute(&i, &thresholds());
    assert_eq!(activity.state, S::Stalled);
    assert!(
        matches!(
            activity.evidence,
            E::Stalled {
                hung: Some(true),
                ..
            }
        ),
        "{:?}",
        activity.evidence
    );
    let json = serde_json::to_value(&activity.evidence).unwrap();
    assert_eq!(json["kind"], "stalled");
    assert_eq!(json["hung"], true);
}

#[test]
fn a_hung_turn_needs_more_than_the_threshold() {
    let mut i = silent_generating_turn();
    i.now -= TimeDelta::seconds(1);
    assert_eq!(activity::compute(&i, &thresholds()).state, S::Working);
}

#[test]
fn tool_phases_are_never_hung_by_this_feature() {
    for phase in ["running_tool", "context_maintenance"] {
        let mut i = silent_generating_turn();
        i.execution.as_mut().unwrap().phase = phase.into();
        assert_eq!(activity::compute(&i, &thresholds()).state, S::Working);
        i.now += TimeDelta::seconds(1200);
        let activity = activity::compute(&i, &thresholds());
        assert_eq!(activity.state, S::Stalled);
        assert!(
            matches!(activity.evidence, E::Stalled { hung: None, .. }),
            "a tool stall must not be reported as a generation hang: {:?}",
            activity.evidence
        );
    }
}

#[test]
fn busy_cpu_or_live_descendants_are_not_hung() {
    let mut i = silent_generating_turn();
    i.process.as_mut().unwrap().cpu_permille = 50;
    assert_eq!(activity::compute(&i, &thresholds()).state, S::Working);
    i.process.as_mut().unwrap().cpu_permille = 0;
    i.process.as_mut().unwrap().descendants = Some(1);
    assert_eq!(activity::compute(&i, &thresholds()).state, S::Working);
    i.now += TimeDelta::hours(2);
    // Живое дерево не исключает обычный `stalled` после старого щедрого порога —
    // но это точно не новый verdict «завис в генерации».
    assert!(matches!(
        activity::compute(&i, &thresholds()).evidence,
        E::Stalled { hung: None, .. }
    ));
}

#[test]
fn a_recent_stream_event_or_a_dead_process_is_not_hung() {
    let mut i = silent_generating_turn();
    i.last_event_at = Some(i.now);
    i.execution.as_mut().unwrap().phase_at = Some(i.now);
    i.turn_started_at = Some(i.now);
    assert_eq!(activity::compute(&i, &thresholds()).state, S::Working);
    let recent = Some(i.now - TimeDelta::seconds(100));
    i.last_event_at = recent;
    i.execution.as_mut().unwrap().phase_at = recent;
    i.turn_started_at = recent;
    i.process.as_mut().unwrap().alive = false;
    assert_eq!(activity::compute(&i, &thresholds()).state, S::Working);
}

#[test]
fn the_hang_threshold_is_configurable() {
    let mut i = silent_generating_turn();
    let custom = Thresholds {
        hung_generating: Duration::from_secs(1200),
        ..thresholds()
    };
    assert_eq!(activity::compute(&i, &custom).state, S::Working);
    i.now += TimeDelta::seconds(600);
    assert_eq!(activity::compute(&i, &custom).state, S::Stalled);
}
