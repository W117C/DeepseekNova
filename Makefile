.PHONY: all build check test clean run release \
        release-patch release-minor release-major \
        check-all test-all test-count test-count-check clippy-fix example \
        install dist audit eval-ci bench-ci fmt cross-linux

# ── Default ─────────────────────────────────────────────────────
all: build

# ── Build ───────────────────────────────────────────────────────
build:
	cargo build

# ── Comprehensive check (CI equivalent) ────────────────────────
check:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test --workspace
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items

check-all:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings || true
	cargo test --workspace

# ── Format ──────────────────────────────────────────────────────
fmt:
	cargo fmt --all

# ── Test ────────────────────────────────────────────────────────
test:
	cargo test --all

test-all:
	cargo test --workspace

test-count:
	python3 scripts/sync-test-count.py

test-count-check:
	python3 scripts/sync-test-count.py --check

# ── Clippy auto-fix ─────────────────────────────────────────────
clippy-fix:
	cargo clippy --workspace --all-targets --fix --allow-dirty

# ── Run ─────────────────────────────────────────────────────────
run:
	cargo run --bin deepseeknova-cli

# ── Example ─────────────────────────────────────────────────────
example:
	cargo run --example quickstart -p deepseeknova-cli

# ── Release build ───────────────────────────────────────────────
release:
	cargo build --release

# ── Version bumping ─────────────────────────────────────────────
release-patch:
	./scripts/bump-version.sh patch

release-minor:
	./scripts/bump-version.sh minor

release-major:
	./scripts/bump-version.sh major

# ── Clean ───────────────────────────────────────────────────────
clean:
	cargo clean

# ── Cross-compilation ──────────────────────────────────────────
cross-linux:
	cross build --target x86_64-unknown-linux-gnu --release

# ── Install CLI binary ─────────────────────────────────────────
install:
	cargo install --path crates/deepseeknova-cli --force

# ── Distribution package ───────────────────────────────────────
dist: release
	@echo "Release binary at target/release/deepseeknova-cli"

# ── Security audit ─────────────────────────────────────────────
audit:
	@command -v cargo-deny >/dev/null 2>&1 || { echo "cargo-deny 未安装，请先安装: cargo install cargo-deny --locked"; exit 1; }
	cargo deny --all-features check

# ── Benchmark with saved baseline（CI bench 基线记录）─────────────────
# 运行 workspace 全部基准，criterion 结果以命名基线 "ci" 保存到
# target/criterion（内部 JSON：estimates.json 等），并打包到
# target/bench-ci/bench.tar.gz 供 CI 上传 artifact 人工对比。
# 注意：criterion 0.8 已移除 --output-format json，基线保存是
# 现版本可用的 JSON 记录手段；未设自动门禁阈值（机器噪声易 flaky），
# 退化由人工比对历史 artifact 发现。
# 参数只定向传给 harness=false 的 criterion bench 目标：`--` 之后的参数
# 会转发给所有 bench 目标，lib 单测的 libtest harness 不认识
# --save-baseline 会报 Unrecognized option（agent --lib 曾因此炸 CI bench）。
# criterion 须启用 cargo_bench_support feature（workspace Cargo.toml），
# 否则 bench 二进制同样不解析该参数。
bench-ci:
	mkdir -p target/bench-ci
	cargo bench -p deepseeknova-core --bench registry --bench events --bench memory_search -- --save-baseline ci
	cargo bench -p deepseeknova-graph --bench retrieval -- --save-baseline ci
	tar czf target/bench-ci/bench.tar.gz -C target criterion
	@echo "基准已保存到 target/criterion（baseline: ci）并打包 target/bench-ci/bench.tar.gz"

# ── Eval 基准（F3：成本优先 CI 门禁）─────────────────────────────
# 跑核心基准任务集并施加 CI 门槛（综合分均值 + 关键维度 + 成本上限）。
# 需要已配置 LLM provider（API key）；无 key 时跳过（not failure）。
eval-ci:
	@test -n "$$DEEPSEEKNOVA_API_KEY" -o -n "$$DEEPSEEK_API_KEY" || { echo "跳过 eval-ci：未配置 LLM API key"; exit 0; }
	cargo run --bin deepseeknova-cli -- eval --path evals/core.jsonl --format json \
		--require-min-score 3.5 --require-dimension governance>=0.7
