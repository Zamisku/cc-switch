//! DeepSeek Harness 的用量导入。
//!
//! 每个 `assistant/message` 事件带一份 `usage`（输入、输出、缓存读、缓存写），
//! 输入不含缓存命中、输出已含思考 token。按 `会话 + turn + step` 去重。

use super::{session_usage::SessionSyncResult, sql_helpers::INPUT_TOKEN_SEMANTICS_FRESH};
use crate::proxy::usage::{calculator::CostCalculator, parser::TokenUsage};
use crate::{
    database::{lock_conn, Database},
    error::AppError,
    session_manager::providers::dsh,
};
use rusqlite::params;
use serde_json::Value;
use std::path::Path;

pub fn sync_dsh_usage(db: &Database) -> Result<SessionSyncResult, AppError> {
    let mut result = SessionSyncResult::default();
    for session in dsh::scan_sessions() {
        let Some(path) = session.source_path else {
            continue;
        };
        result.files_scanned += 1;
        match dsh::usage_records(Path::new(&path)) {
            Ok(records) => match import_records(db, &session.session_id, &records) {
                Ok((imported, skipped)) => {
                    result.imported += imported;
                    result.skipped += skipped;
                }
                Err(error) => result
                    .errors
                    .push(format!("DeepSeek Harness {}: {error}", session.session_id)),
            },
            Err(error) => result.errors.push(error),
        }
    }
    Ok(result)
}

fn token(usage: &Value, key: &str) -> i64 {
    usage
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| i64::try_from(value).ok())
        .unwrap_or(0)
}

/// 五个金额（输入、输出、缓存读、缓存写、合计）；查不到价格的记 0。
fn costs(conn: &rusqlite::Connection, model: &str, tokens: [i64; 4]) -> [String; 5] {
    let [input, output, cache_read, cache_write] =
        tokens.map(|value| u32::try_from(value).unwrap_or(u32::MAX));
    let usage = TokenUsage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_creation_tokens: cache_write,
        cache_creation_1h_tokens: 0,
        model: Some(model.to_string()),
        message_id: None,
    };
    match crate::services::usage_stats::find_model_pricing(conn, model) {
        Some(pricing) => {
            let cost = CostCalculator::calculate_for_app(
                "dsh",
                &usage,
                &pricing,
                rust_decimal::Decimal::from(1),
            );
            [
                cost.input_cost.to_string(),
                cost.output_cost.to_string(),
                cost.cache_read_cost.to_string(),
                cost.cache_creation_cost.to_string(),
                cost.total_cost.to_string(),
            ]
        }
        None => std::array::from_fn(|_| "0".to_string()),
    }
}

fn import_records(db: &Database, session: &str, records: &[Value]) -> Result<(u32, u32), AppError> {
    let mut conn = lock_conn!(db.conn);
    let tx = conn.transaction()?;
    let (mut imported, mut skipped) = (0, 0);
    for record in records {
        let usage = &record["usage"];
        let input = token(usage, "inputTokens");
        let output = token(usage, "outputTokens");
        let cache_read = token(usage, "cacheReadTokens");
        let cache_write = token(usage, "cacheWriteTokens");
        if input == 0 && output == 0 && cache_read == 0 && cache_write == 0 {
            continue;
        }
        let turn = record["turn"].as_u64().unwrap_or(0);
        let step = record["step"].as_u64().unwrap_or(0);
        let id = format!("dsh:{session}:{turn}:{step}");
        let fresh = tx.execute(
            "INSERT OR IGNORE INTO session_usage_dedup(data_source,request_id,semantic_id,has_entry_id) VALUES ('dsh_session',?1,?1,1)",
            [&id],
        )?;
        let model = record["model"]
            .as_str()
            .filter(|model| !model.trim().is_empty())
            .unwrap_or("unknown")
            .to_string();
        let provider = record["provider"].as_str().unwrap_or("dsh");
        let created_at = record["time"]
            .as_i64()
            .map(|millis| millis / 1000)
            .unwrap_or(0);
        let [input_cost, output_cost, cache_cost, write_cost, total_cost] =
            costs(&tx, &model, [input, output, cache_read, cache_write]);
        if fresh == 0 {
            let changed = tx.execute(
                "UPDATE proxy_request_logs SET model=?2,input_tokens=?3,output_tokens=?4,cache_read_tokens=?5,cache_creation_tokens=?6,total_cost_usd=?7 WHERE request_id=?1 AND (model!=?2 OR input_tokens!=?3 OR output_tokens!=?4 OR cache_read_tokens!=?5 OR cache_creation_tokens!=?6 OR total_cost_usd!=?7)",
                params![id, model, input, output, cache_read, cache_write, total_cost],
            )?;
            imported += changed as u32;
            skipped += u32::from(changed == 0);
            continue;
        }
        tx.execute(
            "INSERT INTO proxy_request_logs (
                request_id,provider_id,app_type,model,request_model,input_tokens,output_tokens,
                cache_read_tokens,cache_creation_tokens,input_cost_usd,output_cost_usd,
                cache_read_cost_usd,cache_creation_cost_usd,total_cost_usd,latency_ms,status_code,
                session_id,provider_type,is_streaming,cost_multiplier,created_at,data_source,input_token_semantics
            ) VALUES (?1,'_dsh_session','dsh',?2,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,0,200,?12,'dsh_session',1,'1',?13,'dsh_session',?14)",
            params![
                id,
                model,
                input,
                output,
                cache_read,
                cache_write,
                input_cost,
                output_cost,
                cache_cost,
                write_cost,
                total_cost,
                session,
                created_at,
                INPUT_TOKEN_SEMANTICS_FRESH
            ],
        )?;
        let _ = provider;
        imported += 1;
    }
    tx.commit()?;
    Ok((imported, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(turn: u64, step: u64, input: u64, output: u64, cache: u64) -> Value {
        json!({
            "turn": turn,
            "step": step,
            "time": 1_700_000_000_000i64,
            "usage": {
                "inputTokens": input,
                "outputTokens": output,
                "cacheReadTokens": cache,
                "cacheWriteTokens": 0
            },
            "provider": "deepseek-account",
            "model": "deepseek-flash"
        })
    }

    #[test]
    fn imports_fresh_input_and_deduplicates_by_turn_and_step() {
        let db = Database::memory().unwrap();
        let records = vec![record(1, 1, 1000, 200, 5000)];
        assert_eq!(import_records(&db, "session-a", &records).unwrap(), (1, 0));
        assert_eq!(import_records(&db, "session-a", &records).unwrap(), (0, 1));

        let conn = db.conn.lock().unwrap();
        let got = conn
            .query_row(
                "SELECT input_tokens,output_tokens,cache_read_tokens,model,app_type,input_token_semantics FROM proxy_request_logs",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            got,
            (
                1000,
                200,
                5000,
                "deepseek-flash".to_string(),
                "dsh".to_string(),
                INPUT_TOKEN_SEMANTICS_FRESH
            )
        );
    }

    #[test]
    fn rewritten_usage_updates_retained_rows_without_resurrecting_pruned_ones() {
        let db = Database::memory().unwrap();
        let mut usage = record(1, 1, 10, 2, 0);
        assert_eq!(import_records(&db, "s", &[usage.clone()]).unwrap(), (1, 0));
        usage["usage"]["inputTokens"] = json!(20);
        assert_eq!(import_records(&db, "s", &[usage.clone()]).unwrap(), (1, 0));
        {
            let conn = db.conn.lock().unwrap();
            assert_eq!(
                conn.query_row("SELECT input_tokens FROM proxy_request_logs", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                20
            );
            conn.execute("DELETE FROM proxy_request_logs", []).unwrap();
        }
        assert_eq!(import_records(&db, "s", &[usage]).unwrap(), (0, 1));
    }
}
