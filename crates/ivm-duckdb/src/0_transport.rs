use ivm_engine::{EngineError, ErrorKind, Stage};
use serde_json::Value;
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub statements: u64,
    /// Source changes accepted during this settle.
    pub rows_in: u64,
    /// JSON result rows crossing the process boundary during this settle.
    pub rows_out: u64,
    pub refreshes: u64,
}

pub fn error(stage: Stage, message: impl ToString) -> EngineError {
    EngineError::new(stage, None, ErrorKind::Worker(message.to_string()))
}
pub fn literal(value: &str) -> String {
    if value.contains('\0') {
        format!(
            "decode(from_hex('{}'))",
            value
                .as_bytes()
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect::<String>()
        )
    } else {
        format!("'{}'", value.replace('\'', "''"))
    }
}

fn cli() -> Result<String, EngineError> {
    match std::env::var("IVM_DUCKDB_CLI") {
        Ok(path) => Ok(path),
        Err(std::env::VarError::NotPresent) => Ok("duckdb".into()),
        Err(e) => Err(error(Stage::Install, e)),
    }
}

pub struct Sql {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: File,
    pub dir: tempfile::TempDir,
    pub work: Work,
    serial: u64,
    pub stage: Stage,
    pub diagnostics: String,
}
impl Sql {
    pub fn open() -> Result<Self, EngineError> {
        let dir = tempfile::tempdir().map_err(|e| error(Stage::Install, e))?;
        let path = dir.path().join("stderr");
        let stderr = File::create(&path).map_err(|e| error(Stage::Install, e))?;
        let cli = cli()?;
        let mut child = Command::new(&cli)
            .args(["-unsigned", "-batch", "-json", "-init", "/dev/null"])
            .arg(dir.path().join("state.duckdb"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .map_err(|e| {
                error(
                    Stage::Install,
                    format!("{cli}: {e}; set IVM_DUCKDB_CLI to the matching build"),
                )
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| error(Stage::Install, "missing CLI stdin"))?;
        let stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| error(Stage::Install, "missing CLI stdout"))?,
        );
        let stderr = File::open(path).map_err(|e| error(Stage::Install, e))?;
        let mut sql = Self {
            child,
            stdin,
            stdout,
            stderr,
            dir,
            work: Work::default(),
            serial: 0,
            stage: Stage::Install,
            diagnostics: String::new(),
        };
        sql.configure()?;
        Ok(sql)
    }
    fn configure(&mut self) -> Result<(), EngineError> {
        self.exec("SET threads=4")?;
        let extension = std::env::var("IVM_DUCKDB_EXTENSION").map_err(|_| {
            error(
                Stage::Install,
                "set IVM_DUCKDB_EXTENSION to the matching openivm.duckdb_extension",
            )
        })?;
        self.exec(&format!("LOAD {}", literal(&extension)))?;
        self.exec("SET openivm_disable_daemon=true")?;
        self.exec("SET openivm_cascade_refresh='off'")?;
        self.exec("SET openivm_refresh_mode='incremental'")?;
        self.exec(if matches!(self.stage, Stage::Install) {
            "SET openivm_profile_refresh=true"
        } else {
            "SET openivm_profile_refresh=false"
        })?;
        self.exec(&format!(
            "SET openivm_files_path={}",
            literal(&self.dir.path().to_string_lossy())
        ))?;
        Ok(())
    }
    pub fn checkpoint(&mut self, name: &str) -> Result<(), EngineError> {
        self.exec("CHECKPOINT")?;
        std::fs::copy(
            self.dir.path().join("state.duckdb"),
            self.dir.path().join(name),
        )
        .map_err(|e| error(self.stage, e))?;
        Ok(())
    }
    pub fn restore(&mut self, name: &str) -> Result<(), EngineError> {
        if self
            .child
            .try_wait()
            .map_err(|e| error(self.stage, e))?
            .is_none()
        {
            self.child.kill().map_err(|e| error(self.stage, e))?;
            self.child.wait().map_err(|e| error(self.stage, e))?;
        }
        let database = self.dir.path().join("state.duckdb");
        std::fs::copy(self.dir.path().join(name), &database).map_err(|e| error(self.stage, e))?;
        let wal = self.dir.path().join("state.duckdb.wal");
        if wal.exists() {
            std::fs::remove_file(wal).map_err(|e| error(self.stage, e))?;
        }
        let cli = cli()?;
        self.child = Command::new(cli)
            .args(["-unsigned", "-batch", "-json", "-init", "/dev/null"])
            .arg(database)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(File::create(self.dir.path().join("stderr")).map_err(|e| error(self.stage, e))?)
            .spawn()
            .map_err(|e| error(self.stage, e))?;
        self.stdin = self
            .child
            .stdin
            .take()
            .ok_or_else(|| error(self.stage, "missing stdin"))?;
        self.stdout = BufReader::new(
            self.child
                .stdout
                .take()
                .ok_or_else(|| error(self.stage, "missing stdout"))?,
        );
        self.stderr =
            File::open(self.dir.path().join("stderr")).map_err(|e| error(self.stage, e))?;
        self.configure()
    }
    /// One SQL statement per request; a CLI marker frames an empty or multiline JSON result.
    pub fn exec(&mut self, statement: &str) -> Result<Vec<Value>, EngineError> {
        self.serial += 1;
        self.work.statements += 1;
        let marker = format!("__ivm_end_{}__", self.serial);
        let offset = self
            .stderr
            .seek(SeekFrom::End(0))
            .map_err(|e| error(self.stage, e))?;
        writeln!(self.stdin, "{statement};\n.print {marker}")
            .and_then(|_| self.stdin.flush())
            .map_err(|e| error(self.stage, e))?;
        let mut json = String::new();
        let mut warnings = String::new();
        loop {
            let mut line = String::new();
            if self
                .stdout
                .read_line(&mut line)
                .map_err(|e| error(self.stage, e))?
                == 0
            {
                let mut details = String::new();
                if let Err(e) = self.stderr.read_to_string(&mut details) {
                    details.push_str(&format!("; reading CLI diagnostics failed: {e}"));
                }
                return Err(error(
                    self.stage,
                    format!("DuckDB exited: {details}\nSQL: {statement}"),
                ));
            }
            if line.trim_end() == marker {
                break;
            }
            if line.starts_with("Warning:") {
                warnings.push_str(&line);
            } else {
                json.push_str(&line);
            }
        }
        self.stderr
            .seek(SeekFrom::Start(offset))
            .map_err(|e| error(self.stage, e))?;
        let mut details = String::new();
        self.stderr
            .read_to_string(&mut details)
            .map_err(|e| error(self.stage, e))?;
        details.push_str(&warnings);
        self.diagnostics = details.clone();
        if details.contains("Error:") || details.contains("Exception:") {
            return Err(error(self.stage, format!("{details}\nSQL: {statement}")));
        }
        if !details.trim().is_empty() {
            eprintln!("ivm-duckdb: {}", details.trim_end());
        }
        let rows: Vec<Value> = if json.trim().is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&json).map_err(|e| error(self.stage, format!("{e}: {json}")))?
        };
        self.work.rows_out += rows.len() as u64;
        Ok(rows)
    }
}
impl Drop for Sql {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
