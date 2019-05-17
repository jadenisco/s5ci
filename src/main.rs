use ssh2::Session;
use std::collections::HashMap;
use std::io;
use std::io::prelude::*;
use std::net::TcpStream;
use std::path::Path;

#[macro_use]
extern crate clap;
extern crate exec;
extern crate libc;
extern crate psutil;
extern crate regex;
extern crate signal_hook;
extern crate uuid;
extern crate yaml_rust;
#[macro_use]
extern crate log;
extern crate env_logger;
use clap::{App, Arg, SubCommand};

use regex::Regex;

use std::fs;

#[macro_use]
extern crate serde_derive;
extern crate serde;
extern crate serde_json;
extern crate serde_yaml;

extern crate cron;
extern crate mustache;

use chrono::NaiveDateTime;

mod gerrit_types;
mod gerrit_interact;
mod s5ci_config;
mod run_ssh_command;
mod unix_process;
mod runtime_data;
mod database;
mod comment_triggers;
mod html_gen;
mod job_mgmt;

use crate::gerrit_types::*;
use crate::s5ci_config::*;
use crate::run_ssh_command::*;
use crate::unix_process::*;
use crate::runtime_data::*;
use crate::gerrit_interact::*;
use crate::database::*;
use crate::comment_triggers::*;
use crate::html_gen::*;
use crate::job_mgmt::*;

use s5ci::*;


fn process_change(
    config: &s5ciConfig,
    rtdt: &s5ciRuntimeData,
    cs: &GerritChangeSet,
    before_when: Option<NaiveDateTime>,
    after_when: Option<NaiveDateTime>,
) {
    let mut triggers: Vec<CommentTrigger> = vec![];
    let mut max_pset = 0;

    // eprintln!("Processing change: {:#?}", cs);
    if let Some(startline) = after_when {
        let startline_ts =
            startline.timestamp() - 1 + config.default_regex_trigger_delay_sec.unwrap_or(0) as i64;

        debug!("process change with startline timestamp: {}", startline.timestamp());
        debug!("process change with startline_ts: {}", &startline_ts);

        let mut psmap: HashMap<String, GerritPatchSet> = HashMap::new();

        if let Some(psets) = &cs.patchSets {
            for pset in psets {
                if pset.createdOn > 0 {
                    // startline_ts {
                    // println!("{:?}", &pset);
                    debug!(
                        "  #{} revision: {} ref: {}",
                        &pset.number, &pset.revision, &pset.r#ref
                    );
                    // spawn_command_x("scripts", "git-test", &pset.r#ref);
                }
                psmap.insert(format!("{}", &pset.number), pset.clone());
                psmap.insert(format!("{}", &pset.revision), pset.clone());
                if pset.number > max_pset {
                    max_pset = pset.number;
                }
            }

            // eprintln!("Patchset map: {:#?}", &psmap);
        }
        if let Some(comments_vec) = &cs.comments {
            let change_id = cs.number.unwrap() as i32;
            let all_triggers = get_comment_triggers_from_comments(
                config,
                rtdt,
                change_id,
                max_pset,
                comments_vec,
                startline_ts,
            );
            let mut final_triggers = all_triggers.clone();
            let mut suppress_map: HashMap<(String, u32), bool> = HashMap::new();
            for mut ctrig in final_triggers.iter_mut().rev() {
                let key = (ctrig.trigger_name.clone(), ctrig.patchset_id);
                if ctrig.is_suppress {
                    suppress_map.insert(key, true);
                } else if suppress_map.contains_key(&key) {
                    ctrig.is_suppressed = true;
                    suppress_map.remove(&key);
                }
            }
            if let Some(cfgt) = &config.triggers {
                final_triggers.retain(|x| {
                    let ctrig = &cfgt[&x.trigger_name];
                    let mut retain = !x.is_suppressed;
                    if let Some(proj) = &ctrig.project {
                        if let Some(cs_proj) = &cs.project {
                            if cs_proj != proj {
                                retain = false;
                            }
                        } else {
                            retain = false;
                        }
                    }
                    if let s5TriggerAction::command(cmd) = &ctrig.action {
                        retain
                    } else {
                        false
                    }
                });
                // now purge all the suppressing triggers themselves
                final_triggers.retain(|x| !x.is_suppress);
            }
            // eprintln!("all triggers: {:#?}", &final_triggers);
            eprintln!("final triggers: {:#?}", &final_triggers);
            for trig in &final_triggers {
                let template = rtdt
                    .trigger_command_templates
                    .get(&trig.trigger_name)
                    .unwrap();
                let mut data = mustache::MapBuilder::new();
                if let Some(patchset) = psmap.get(&format!("{}", trig.patchset_id)) {
                    data = data.insert("patchset", &patchset).unwrap();
                }
                data = data.insert("regex", &trig.captures).unwrap();
                let data = data.build();
                let mut bytes = vec![];

                template.render_data(&mut bytes, &data).unwrap();
                let expanded_command = String::from_utf8_lossy(&bytes);
                let change_id = cs.number.unwrap();
                let mut rtdt2 = rtdt.clone();
                rtdt2.changeset_id = Some(change_id);
                rtdt2.patchset_id = Some(trig.patchset_id);
                if (trig.is_suppress || trig.is_suppressed) {
                    panic!(format!("bug: job is not runnable: {:#?}", &trig));
                }
                let job_id = spawn_command(config, &rtdt2, &expanded_command);
            }
        }
    }
}

fn do_gerrit_command(config: &s5ciConfig, rtdt: &s5ciRuntimeData, cmd: &str) {
    run_ssh_command(config, cmd);
}

fn do_review(
    config: &s5ciConfig,
    rtdt: &s5ciRuntimeData,
    maybe_vote: &Option<GerritVoteAction>,
    msg: &str,
) {
    gerrit_add_review_comment(config, rtdt, maybe_vote, msg)
}

fn do_list_jobs(config: &s5ciConfig, rtdt: &s5ciRuntimeData) {
    let jobs = db_get_all_jobs();
    for j in jobs {
        if j.finished_at.is_some() {
            // show jobs finished up to 10 seconds ago
            let ndt_horizon = ndt_add_seconds(now_naive_date_time(), -10);
            if j.finished_at.clone().unwrap() < ndt_horizon {
                continue;
            }
        }
        println!("{:#?}", &j);
    }
}
fn do_kill_job(
    config: &s5ciConfig,
    rtdt: &s5ciRuntimeData,
    jobid: &str,
    terminator: &str,
) {
    let job = db_get_job(jobid).unwrap();
    if job.finished_at.is_none() {
        if let Some(pid) = job.command_pid {
            println!(
                "Requested to kill a job, sending signal to pid {} from job {:?}",
                pid, &job
            );
            kill_process(pid);
            do_set_job_status(
                config,
                rtdt,
                &job.job_id,
                &format!("Terminated by the {}", terminator),
            );
        }
    }
}

fn do_run_job(config: &s5ciConfig, rtdt: &s5ciRuntimeData, args: &s5ciRunJobArgs) {
    use signal_hook::{iterator::Signals, SIGABRT, SIGHUP, SIGINT, SIGPIPE, SIGQUIT, SIGTERM};
    use std::{error::Error, thread};
    let cmd = &args.cmd;

    let signals = Signals::new(&[SIGINT, SIGPIPE, SIGHUP, SIGQUIT, SIGABRT, SIGTERM]).unwrap();

    thread::spawn(move || {
        for sig in signals.forever() {
            println!("Received signal {:?}", sig);
        }
    });
    println!("Requested to run job '{}'", cmd);
    let group_name = job_group_name_from_cmd(cmd);
    let jobs = db_get_jobs_by_group_name_and_csps(
        &group_name,
        rtdt.changeset_id.unwrap(),
        rtdt.patchset_id.unwrap(),
    );
    if args.omit_if_ok {
        if jobs.len() > 0 {
            if jobs[0].return_success {
                println!("Requested to omit if success, existing success job: {:?}, exit no-op with success", &jobs[0]);
                std::process::exit(0);
            }
        }
    }
    if args.kill_previous {
        if jobs.len() > 0 && jobs[0].finished_at.is_none() {
            do_kill_job(config, rtdt, &jobs[0].job_id, "next job");
        }
    }
    let (job_id, status) = exec_command(config, rtdt, cmd);
    let mut ret_status = 4242;
    if let Some(st) = status {
        ret_status = st;
    }
    println!("Exiting job '{}' with status {}", cmd, &ret_status);
    std::process::exit(ret_status);
}

fn do_set_job_status(
    config: &s5ciConfig,
    rtdt: &s5ciRuntimeData,
    a_job_id: &str,
    a_msg: &str,
) {
    let j = db_get_job(a_job_id);
    if j.is_err() {
        error!("Could not find job {}", a_job_id);
        return;
    }
    let j = j.unwrap();

    {
        use diesel::expression_methods::*;
        use diesel::query_dsl::QueryDsl;
        use diesel::query_dsl::RunQueryDsl;
        use schema::jobs;
        use schema::jobs::dsl::*;

        let some_ndt_now = Some(now_naive_date_time());
        let db = get_db();

        let updated_rows = diesel::update(jobs.filter(job_id.eq(&a_job_id)))
            .set((
                status_message.eq(a_msg.to_string()),
                status_updated_at.eq(some_ndt_now),
            ))
            .execute(db.conn())
            .unwrap();
    }
    regenerate_job_html(config, rtdt, &a_job_id);
}

fn restart_ourselves() {
    use std::env;
    use std::process;
    let argv_real: Vec<String> = env::args().collect();
    let err = exec::Command::new(&argv_real[0])
        .args(&argv_real[1..])
        .exec();
    // normally not reached
    println!("Error: {}", err);
    process::exit(1);
}

fn get_mtime(fname: &str) -> Option<std::time::SystemTime> {
    let mtime = fs::metadata(fname).ok().map(|x| x.modified().unwrap());
    mtime
}

fn file_changed_since(fname: &str, since: Option<std::time::SystemTime>) -> bool {
    use std::time::{Duration, SystemTime};
    let new_mtime = get_mtime(fname);
    let few_seconds = Duration::from_secs(10);
    if let (Some(old_t), Some(new_t)) = (since, new_mtime) {
        if new_t.duration_since(old_t).unwrap_or(few_seconds) > few_seconds {
            return true;
        }
    }
    // be conservative if we didn't have either of mtimes
    return false;
}

fn process_cron_triggers(
    config: &s5ciConfig,
    rtdt: &s5ciRuntimeData,
    since: &NaiveDateTime,
    now: &NaiveDateTime,
) -> NaiveDateTime {
    // use chrono::Local;
    use chrono::{DateTime, Local, TimeZone};

    let dt_since = Local.from_local_datetime(&since).unwrap();
    let ndt_max_cron = ndt_add_seconds(now.clone(), 3600 * 24); /* within 24h we will surely have a poll */
    let dt_max_cron = Local.from_local_datetime(&ndt_max_cron).unwrap();
    let mut dt_now = Local.from_local_datetime(&now).unwrap();
    let mut dt_next_cron = Local.from_local_datetime(&ndt_max_cron).unwrap();

    for sched in &rtdt.cron_trigger_schedules {
        let mut skip = 0;
        let next_0 = sched.schedule.after(&dt_since).nth(0);
        println!("NEXT {} cron: {:?}", &sched.name, &next_0);
        let next_0 = next_0.unwrap_or(dt_max_cron.clone());
        if (next_0 < dt_now) {
            // run cron command
            debug!("CRON: attempting to run {}", &sched.name);
            if let Some(triggers) = &config.cron_triggers {
                if let Some(ctrig) = triggers.get(&sched.name) {
                    if let s5TriggerAction::command(cmd) = &ctrig.action {
                        let job_id = spawn_command(config, rtdt, &cmd);
                    }
                }
            }
            skip = 1;
        } else {
            debug!(
                "CRON not running {} as {} is in the future",
                &sched.name, &next_0
            );
        }
        for d in sched.schedule.after(&dt_since).skip(skip) {
            if d < dt_now {
                /* in the past, no need to deal with this one */
                continue;
            }
            if d > dt_next_cron {
                /* later than next cron, stop looking */
                break;
            }
            dt_next_cron = d;
        }
    }
    let ndt_next_cron = dt_next_cron.naive_local();
    debug!("CRON: Next cron occurence: {}", &ndt_next_cron);
    return ndt_add_seconds(ndt_next_cron, -1); /* one second earlier to catch the next occurence */
}

fn do_loop(config: &s5ciConfig, rtdt: &s5ciRuntimeData) {
    use std::env;
    use std::fs;
    println!("Starting loop at {}", now_naive_date_time());
    regenerate_all_html(&config, &rtdt);

    let sync_horizon_sec: u32 = config
        .server
        .sync_horizon_sec
        .unwrap_or(config.default_sync_horizon_sec.unwrap_or(86400));

    let mut before: Option<NaiveDateTime> = None;
    let mut after: Option<NaiveDateTime> = Some(NaiveDateTime::from_timestamp(
        (now_naive_date_time().timestamp() - sync_horizon_sec as i64),
        0,
    ));

    let mut cron_timestamp = now_naive_date_time();
    let mut poll_timestamp = now_naive_date_time();
    let config_mtime = get_mtime(&rtdt.config_path);
    let exe_mtime = get_mtime(&rtdt.real_s5ci_exe);

    if let Some(trigger_delay_sec) = config.default_regex_trigger_delay_sec {
        println!("default_regex_trigger_delay_sec = {}, all regex trigger reactions will be delayed by that", trigger_delay_sec)
    }

    loop {
        if let Some(trigger_delay_sec) = config.default_regex_trigger_delay_sec {
            if let Some(after_ts) = after {
                after = Some(ndt_add_seconds(after_ts, -(trigger_delay_sec as i32)));
            }
        }
        if config.autorestart.on_config_change
            && file_changed_since(&rtdt.config_path, config_mtime)
        {
            println!(
                "Config changed, attempt restart at {}...",
                now_naive_date_time()
            );
            restart_ourselves();
        }
        if config.autorestart.on_exe_change && file_changed_since(&rtdt.real_s5ci_exe, exe_mtime)
        {
            println!(
                "Executable changed, attempt restart at {}... ",
                now_naive_date_time()
            );
            restart_ourselves();
        }
        let ndt_now = now_naive_date_time();
        if ndt_now > poll_timestamp {
            // println!("{:?}", ndt);
            let res_res = poll_gerrit_over_ssh(&config, &rtdt, before, after);
            if let Ok(res) = res_res {
                for cs in res.changes {
                    process_change(&config, &rtdt, &cs, before, after);
                }
                before = res.before_when;
                after = res.after_when;
                if let Some(before_time) = before.clone() {
                    if before_time.timestamp()
                        < now_naive_date_time().timestamp() - sync_horizon_sec as i64
                    {
                        eprintln!(
                            "Time {} is beyond the horizon of {} seconds from now, finish sync",
                            &before_time, sync_horizon_sec
                        );
                        before = None;
                    }
                }
            } else {
                eprintln!("Error doing ssh: {:?}", &res_res);
            }
            let mut wait_time_ms = config.server.poll_wait_ms.unwrap_or(300000);
            if before.is_some() {
                wait_time_ms = config.server.syncing_poll_wait_ms.unwrap_or(wait_time_ms);
            }
            poll_timestamp = ndt_add_ms(poll_timestamp, wait_time_ms as i64 - 10);
        } else {
            debug!(
                "Poll timestamp {} is in the future, not polling",
                &poll_timestamp
            );
        }

        if ndt_now > ndt_add_seconds(cron_timestamp, 1) {
            cron_timestamp = process_cron_triggers(config, rtdt, &cron_timestamp, &ndt_now);
        } else {
            debug!(
                "Cron timestamp {} is in the future, no cron processing this time",
                &cron_timestamp
            );
        }

        let mut next_timestamp = ndt_add_seconds(cron_timestamp, 2);
        if poll_timestamp < next_timestamp {
            next_timestamp = poll_timestamp;
        }

        let wait_time_ms = next_timestamp
            .signed_duration_since(now_naive_date_time())
            .num_milliseconds()
            + 1;

        collect_zombies();
        // ps();
        // eprintln!("Sleeping for {} msec ({})", wait_time_ms, wait_name);
        debug!("Sleeping for {} ms", wait_time_ms);
        if wait_time_ms > 0 {
            s5ci::thread_sleep_ms(wait_time_ms as u64);
        }
    }
}

fn main() {
    env_logger::init();
    let (config, rtdt) = get_configs();
    use s5ciAction;
    maybe_compile_template(&config, "job_page").unwrap();
    maybe_compile_template(&config, "root_job_page").unwrap();
    maybe_compile_template(&config, "active_job_page").unwrap();
    maybe_compile_template(&config, "group_job_page").unwrap();

    match &rtdt.action {
        s5ciAction::Loop => do_loop(&config, &rtdt),
        s5ciAction::ListJobs => do_list_jobs(&config, &rtdt),
        s5ciAction::KillJob(job_id) => do_kill_job(&config, &rtdt, &job_id, "S5CI CLI"),
        s5ciAction::RunJob(cmd) => do_run_job(&config, &rtdt, &cmd),
        s5ciAction::SetStatus(job_id, msg) => do_set_job_status(&config, &rtdt, &job_id, &msg),
        s5ciAction::GerritCommand(cmd) => do_gerrit_command(&config, &rtdt, &cmd),
        s5ciAction::MakeReview(maybe_vote, msg) => do_review(&config, &rtdt, maybe_vote, &msg),
    }
}
