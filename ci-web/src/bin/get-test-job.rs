extern crate libc;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{OpenOptions, create_dir_all};
use std::io::ErrorKind;
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use std::process;
use std::process::Stdio;
use std::time::SystemTime;
use ci_cgi::{Ktestrc, KtestrcTestGroup, ktestrc_read, git_get_commit, commitdir_get_results};
use ci_cgi::{Worker, workers_update};
use ci_cgi::TestResultsMap;
use die::die;
use file_lock::{FileLock, FileOptions};
use memoize::memoize;
use anyhow;
use clap::Parser;
use chrono::Utc;

#[memoize]
fn get_subtests(test_path: PathBuf) -> Vec<String> {
    let output = std::process::Command::new(&test_path)
        .arg("list-tests")
        .output()
        .expect(&format!("failed to execute process {:?} ", &test_path))
        .stdout;
    let output = String::from_utf8_lossy(&output);

    output
        .split_whitespace()
        .map(|i| i.to_string())
        .collect()
}

fn lockfile_exists(rc: &Ktestrc, commit: &str, test_name: &str, create: bool) -> bool {
    let lockfile = rc.output_dir.join(commit).join(test_name).join("status");

    let timeout = std::time::Duration::from_secs(3600);
    let metadata = std::fs::metadata(&lockfile);

    if let Ok(metadata) = metadata {
        let elapsed = metadata.modified().unwrap()
            .elapsed()
            .unwrap_or(std::time::Duration::from_secs(0));

        if metadata.is_file() &&
           metadata.len() == 0 &&
           elapsed > timeout &&
           std::fs::remove_file(&lockfile).is_ok() {
            eprintln!("Deleted stale lock file {:?}, mtime {:?} now {:?} elapsed {:?})",
                      &lockfile, metadata.modified().unwrap(),
                      SystemTime::now(),
                      elapsed);
        }
    }

    if !create {
        lockfile.exists()
    } else {
        let dir = lockfile.parent().unwrap();
        let r = create_dir_all(dir);
        if let Err(e) = r {
            if e.kind() != ErrorKind::AlreadyExists {
                die!("error creating {:?}: {}", dir, e);
            }
        }

        let r = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lockfile);
        if let Err(ref e) = r {
            if e.kind() != ErrorKind::AlreadyExists {
                die!("error creating {:?}: {}", lockfile, e);
            }
        }

        r.is_ok()
    }
}

#[derive(Debug)]
struct TestJob {
    branch:     String,
    commit:     String,
    age:        u64,
    priority:   u64,
    test:       PathBuf,
    subtests:   Vec<String>,
}

fn testjob_weight(j: &TestJob) -> u64 {
    j.age + j.priority
}

use std::cmp::Ordering;

impl Ord for TestJob {
    fn cmp(&self, other: &Self) -> Ordering {
        testjob_weight(self).cmp(&testjob_weight(other))
    }
}

impl PartialOrd for TestJob {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

impl PartialEq for TestJob {
    fn eq(&self, other: &Self) -> bool { self.cmp(other) == Ordering::Equal }
}

impl Eq for TestJob {}

fn subtest_full_name(test_path: &Path, subtest: &String) -> String {
    format!("{}.{}",
            test_path.file_stem().unwrap().to_string_lossy(),
            subtest.replace("/", "."))
}

fn have_result(results: &TestResultsMap, subtest: &str) -> bool {
    use ci_cgi::TestStatus;

    let r = results.get(subtest);
    if let Some(r) = r {
        let elapsed = Utc::now() - r.starttime;
        let timeout = chrono::Duration::minutes(30);

        r.status != TestStatus::Inprogress || elapsed < timeout
    } else {
        false
    }
}

fn branch_get_next_test_job(args: &Args, rc: &Ktestrc, repo: &git2::Repository,
                            branch: &str,
                            test_group: &KtestrcTestGroup,
                            test_path: &Path) -> Option<TestJob> {
    let test_path = rc.ktest_dir.join("tests").join(test_path);
    let mut ret =  TestJob {
        branch:     branch.to_string(),
        commit:     String::new(),
        age:        0,
        priority:   test_group.priority,
        test:       test_path.to_path_buf(),
        subtests:   Vec::new(),
    };

    let subtests = get_subtests(test_path.clone());

    if args.verbose { eprintln!("looking for tests to run for branch {} test {:?} subtests {:?}",
        branch, test_path, subtests) }

    let mut walk = repo.revwalk().unwrap();
    let reference = git_get_commit(&repo, branch.to_string());
    if reference.is_err() {
        eprintln!("branch {} not found", branch);
        return None;
    }
    let reference = reference.unwrap();

    if let Err(e) = walk.push(reference.id()) {
        eprintln!("Error walking {}: {}", branch, e);
        return None;
    }

    for (age, commit) in walk
            .filter_map(|i| i.ok())
            .filter_map(|i| repo.find_commit(i).ok())
            .take(test_group.max_commits as usize)
            .enumerate() {
        let commit = commit.id().to_string();
        ret.commit = commit.clone();
        ret.age = age as u64;

        let results = commitdir_get_results(rc, &commit).unwrap_or(BTreeMap::new());

        if args.verbose { eprintln!("at commit {} age {}\nresults {:?}",
            &commit, age, results) }

        for subtest in subtests.iter() {
            let full_subtest_name = subtest_full_name(&test_path, &subtest);

            if !have_result(&results, &full_subtest_name) &&
               !lockfile_exists(rc, &commit, &full_subtest_name, false) {
                ret.subtests.push(subtest.to_string());
                if ret.subtests.len() > 20 {
                    break;
                }
            }
        }

        if !ret.subtests.is_empty() {
            if args.verbose { eprintln!("possible job: {:?}", ret) }
            return Some(ret);
        }
    }

    None
}

fn get_best_test_job(args: &Args, rc: &Ktestrc, repo: &git2::Repository) -> Option<TestJob> {
    rc.branch.iter()
        .flat_map(move |(branch, branchconfig)| branchconfig.tests.iter()
            .filter_map(|i| rc.test_group.get(i)).map(move |testgroup| (branch, testgroup)))
        .flat_map(move |(branch, testgroup)| testgroup.tests.iter()
            .filter_map(move |test| branch_get_next_test_job(args, rc, repo, &branch, &testgroup, &test)))
        .min()
}

fn create_job_lockfiles(rc: &Ktestrc, mut job: TestJob) -> Option<TestJob> {
    job.subtests = job.subtests.iter()
        .filter(|i| lockfile_exists(rc, &job.commit,
                                    &subtest_full_name(&Path::new(&job.test), &i), true))
        .map(|i| i.to_string())
        .collect();

    if !job.subtests.is_empty() { Some(job) } else { None }
}

fn get_and_lock_job(args: &Args, rc: &Ktestrc, repo: &git2::Repository) -> Option<TestJob> {
    loop {
        let job = get_best_test_job(args, rc, repo);

        if job.is_none() {
            return job;
        }

        let job = create_job_lockfiles(rc, job.unwrap());
        if job.is_some() {
            return job;
        }
    }
}

fn fetch_remotes(rc: &Ktestrc, repo: &git2::Repository) -> anyhow::Result<()> {
    fn fetch_remotes_locked(rc: &Ktestrc, repo: &git2::Repository) -> Result<(), git2::Error> {
        for (branch, branchconfig) in &rc.branch {
            let fetch = branchconfig.fetch
                .split_whitespace()
                .map(|i| OsStr::new(i));

            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&rc.linux_repo)
                .arg("fetch")
                .args(fetch)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .status()
                .expect(&format!("failed to execute fetch"));
            if !status.success() {
                eprintln!("fetch error: {}", status);
                return Ok(());
            }

            let fetch_head = repo.revparse_single("FETCH_HEAD")
                .map_err(|e| { eprintln!("error parsing FETCH_HEAD: {}", e); e})?
                .peel_to_commit()
                .map_err(|e| { eprintln!("error getting FETCH_HEAD: {}", e); e})?;

            repo.branch(branch, &fetch_head, true)?;
        }

        Ok(())
    }

    let lockfile = ".git_fetch.lock";
    let metadata = std::fs::metadata(&lockfile);
    if let Ok(metadata) = metadata {
        let elapsed = metadata.modified().unwrap()
            .elapsed()
            .unwrap_or_default();

        if elapsed < std::time::Duration::from_secs(30) {
            return Ok(());
        }
    }

    let mut filelock = FileLock::lock(lockfile, false, FileOptions::new().create(true).write(true))?;

    eprint!("Fetching remotes...");
    fetch_remotes_locked(rc, repo)?;
    eprintln!(" done");

    filelock.file.write_all(b"ok")?; /* update lockfile mtime */
    Ok(())
}

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    dry_run:    bool,

    #[arg(short, long)]
    verbose:    bool,

    hostname:   String,
    workdir:    String,
}

fn main() {
    let args = Args::parse();

    let ktestrc = ktestrc_read();
    if let Err(e) = ktestrc {
        eprintln!("could not read config; {}", e);
        process::exit(1);
    }
    let ktestrc = ktestrc.unwrap();

    let repo = git2::Repository::open(&ktestrc.linux_repo);
    if let Err(e) = repo {
        eprintln!("Error opening {:?}: {}", ktestrc.linux_repo, e);
        eprintln!("Please specify correct linux_repo");
        process::exit(1);
    }
    let repo = repo.unwrap();

    fetch_remotes(&ktestrc, &repo).ok();

    let job = if !args.dry_run {
        get_and_lock_job(&args, &ktestrc, &repo)
    } else {
        get_best_test_job(&args, &ktestrc, &repo)
    };

    if let Some(job) = job {
        let tests = job.test.into_os_string().into_string().unwrap() + " " + &job.subtests.join(" ");

        println!("TEST_JOB {} {} {}", job.branch, job.commit, tests);

        workers_update(&ktestrc, Worker {
            hostname:   args.hostname,
            workdir:    args.workdir,
            starttime:  Utc::now(),
            branch:     job.branch.clone(),
            age:        job.age,
            commit:     job.commit.clone(),
            tests:      tests.clone(),
        });
    } else {
        workers_update(&ktestrc, Worker {
            hostname:   args.hostname,
            workdir:    args.workdir,
            starttime:  Utc::now(),
            branch:     "".to_string(),
            age:        0,
            commit:     "".to_string(),
            tests:      "".to_string(),
        });
    }
}
