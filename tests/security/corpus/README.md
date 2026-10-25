# 解码面语料（G17-04）

这里的 `.hex` 是**历史崩溃/可疑样本的最小化形式**。存成 hex 而不是二进制，
是为了能在 diff 里看清"改的是哪个字节"。

## 规则

1. **样本不可改。** 为了跑通去改样本，等于把测出来的 bug 删掉。
   如果样本本身不合法（比如它其实是个良性包），在 `manifest.json` 里改 `verdict`
   并写明理由——那是一次可见的决定，而不是一次静默的编辑。
2. **崩溃样本必须可追责。** `verdict: blocked` 的条目必须带 `blocker` + `issue`，
   并在 `security/release-reviews/blockers.json` 里登记（G17-05 会核对）。
3. **只收最小化样本。** 一个几十 KB 的包只会让人跳过不看；能砍到 20 字节就砍到 20 字节。

## 当前样本

| 文件 | 字节 | 裁决 | 说明 |
|---|---:|---|---|
| `s01_empty.hex` | 0 | reject | 空 blob |
| `s02_bad_magic.hex` | 20 | reject | 魔数被替换 |
| `s03_len_mismatch.hex` | 20 | **blocked** | `payload_len = u64::MAX` → 整数溢出 panic（#229） |
| `s04_garbage_payload.hex` | 28 | reject | 长度自洽但 payload 不是 zstd |
| `s05_version2.hex` | 20 | reject | 未支持版本 |

## 加一个样本

```bash
# 1) 把命中样本存成 hex（一行或带注释都可以，读取时只认 hex 字符）
python - <<'PY'
import pathlib, binascii
pathlib.Path('tests/security/corpus/s06_xxx.hex').write_text(binascii.hexlify(pathlib.Path('crash.bin').read_bytes()).decode())
PY
# 2) 在 manifest.json 的 samples 里加一条：file / verdict / size / why（blocked 还要 blocker + issue）
# 3) cargo test --manifest-path tests/security/Cargo.toml --test fuzz
```

变异 fuzz（`mutation_fuzz_only_hits_the_known_overflow`）会在 CI 里跑 2000 次变异，
只允许已登记形态的 panic。长跑版由 nightly 定时任务跑（见
`integration-requests/G17/G17-04.yaml` 里的 workflow 片段）。
