use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

pub struct FfmpegFixture {
    dir: PathBuf,
}

impl FfmpegFixture {
    pub fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("video-utils-rs-{name}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    pub fn path(&self, filename: &str) -> PathBuf {
        self.dir.join(filename)
    }

    pub fn read(&self, filename: &str) -> Vec<u8> {
        fs::read(self.path(filename)).unwrap()
    }

    pub fn run_ffmpeg(&self, args: &[&str]) {
        let output = Command::new("ffmpeg")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("run ffmpeg");
        assert!(
            output.status.success(),
            "ffmpeg failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

impl Drop for FfmpegFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

pub fn path_str(path: &Path) -> &str {
    path.to_str().expect("fixture path is valid UTF-8")
}
