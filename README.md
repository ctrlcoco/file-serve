# file-serve

Transfer files via LAN, pointlessly written in rust with [axum](https://docs.rs/axum/latest/axum/).

---

## Usage

```
Usage: file-serve [OPTIONS]

Options:
  -p, --port <P>    Server port
  -f, --folder <f>  Share folder
  -h, --help        Print help
  -V, --version     Print version
```

Navigate to the shown link. 

---

## Build from source

1. Install the Rust toolchain using [rustup](https://rustup.rs/).

2. Clone the repo.
   ```
   git clone https://github.com/ctrlcoco/file-serve.git
   cd file-serve
   ```
3. Build the program.
   ```
   cargo build --release
   ```

---

## Reporting a Vulnerability

If you want to disclose a vulnerability, please email
to [cocosoftware8@gmail.com]("mailto:cocosoftware8@gmail.com?subject=Vulnerability")
