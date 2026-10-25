//! Инструменты: успешный вызов и вызов с ошибкой, связь tool_use ↔ tool_result,
//! `is_error`, сборка частичного JSON входа из потока.

use std::time::Duration;

use serde_json::json;

use super::{Ctx, TURN_TIMEOUT, result_summary};
use crate::broker::deny_all;
use crate::config::Config;

pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let mut ctx = Ctx::new(
        cfg,
        "tools",
        deny_all("tools scenario: no permission prompt expected for read-only actions"),
    )
    .await?;
    std::fs::write(ctx.cwd.join("hello.txt"), "hello from canterlot probe\n")?;
    let mut s = ctx.spawn(&[], &[])?;

    ctx.step("turn: one successful read-only tool call and one failing tool call");
    ctx.turns += 1;
    let out = s
        .turn(
            "Make exactly two tool calls, one after another. First, use the Bash tool to run `ls -a` in the current directory. \
             Second, use the Read tool to read the file `does-not-exist.txt` in the current directory. \
             After both calls, reply with one short sentence about what you saw.",
            TURN_TIMEOUT,
        )
        .await?;
    let r = out.result.clone().unwrap_or_default();

    let uses: Vec<_> = s.tools.uses().to_vec();
    let linked: Vec<_> = uses
        .iter()
        .map(|u| {
            let res = s.tools.result_for(&u.id);
            json!({"tool_use_id": u.id, "name": u.name, "input": u.input,
                   "result_is_error": res.map(|x| x.is_error),
                   "result_text_head": res.map(|x| x.text.chars().take(300).collect::<String>())})
        })
        .collect();
    let bash_ok = uses.iter().any(|u| {
        u.name == "Bash"
            && s.tools
                .result_for(&u.id)
                .is_some_and(|x| !x.is_error && x.text.contains("hello.txt"))
    });
    let read_err = uses
        .iter()
        .any(|u| u.name == "Read" && s.tools.result_for(&u.id).is_some_and(|x| x.is_error));
    let unmatched = s.tools.unmatched_uses();
    let anomalies = s.tools.orphan_or_duplicate_results();
    let assembled: Vec<_> = s
        .tool_inputs
        .done
        .iter()
        .map(|a| {
            json!({"tool_use_id": a.tool_use_id, "name": a.name, "delta_pieces": a.pieces,
                        "parsed": a.input.as_ref().ok(), "parse_error": a.input.as_ref().err()})
        })
        .collect();
    let streamed_inputs_match = s.tool_inputs.done.iter().all(|a| {
        uses.iter()
            .find(|u| u.id == a.tool_use_id)
            .is_some_and(|u| a.input.as_ref().ok() == Some(&u.input))
    }) && !s.tool_inputs.done.is_empty();
    let perms_seen: Vec<_> = std::iter::from_fn(|| ctx.perms.try_recv().ok())
        .map(|p| p.arguments)
        .collect();

    let linkage_ok = ctx.check(
        "tool_use_result_linkage",
        bash_ok && read_err && unmatched.is_empty() && anomalies.is_empty(),
        json!({"tool_calls": linked, "unmatched_tool_uses": unmatched, "orphan_or_duplicate_results": anomalies}),
    );
    ctx.check(
        "partial_tool_input_json",
        streamed_inputs_match,
        json!({"assembled_from_input_json_delta": assembled,
               "unfinished_blocks_at_turn_end": s.tool_inputs.pending()}),
    );
    ctx.check(
        "no_permission_prompt_for_read_only",
        perms_seen.is_empty(),
        json!({"permission_requests": perms_seen}),
    );
    let result_ok = ctx.check(
        "turn_result_after_tool_error",
        r["subtype"] == "success",
        json!({"result": result_summary(&r), "delivery": s.delivery_note(&out.uuid)}),
    );

    s.p.close_stdin();
    s.pump_for(Duration::from_secs(5)).await;
    let status = s.p.wait_exit(Duration::from_secs(20)).await;
    ctx.rec
        .note(json!({"exit_after_eof": status.map(|st| crate::process::exit_json(&st))}));
    ctx.rec.note(json!({"stream_hygiene": s.hygiene_note()}));

    let ok = linkage_ok && result_ok;
    ctx.finish(
        if ok { "PASS" } else { "FAIL" },
        if ok {
            "successful Bash and failing Read observed as tool_use/tool_result pairs linked by id; is_error=true on the failure; turn completed"
        } else {
            "see check notes"
        },
    );
    Ok(())
}
