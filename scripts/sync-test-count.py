#!/usr/bin/env python3
"""按 Linux CI 的 `cargo test --all` passed 总数同步 README 测试数（徽章 + 表格）。

用法：
  scripts/sync-test-count.py              # 运行 cargo test --all 并更新 README
  scripts/sync-test-count.py --check      # 只校验 README 数字，不写入（CI 用）
  scripts/sync-test-count.py --log FILE   # 复用已保存的 cargo test 日志
  scripts/sync-test-count.py --dry-run    # 显示将要写入的变更，不落盘

权威口径：README 测试数 = Linux CI 上 `cargo test --all` 的 passed 总数
（`deepseeknova-sandbox` 含 Linux 专属测试，本地 macOS/Windows 运行结果
可能少于此值）。因此非 Linux 平台默认拒绝覆盖，仅提示。
"""
import argparse
import platform
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
README = REPO / "README.md"
README_EN = REPO / "README_EN.md"

BADGE_RE = re.compile(r"tests-(\d+)-brightgreen\.svg")
CN_TABLE_RE = re.compile(r"\| 测试 \| (\d+) tests · cargo-llvm-cov · CI 三平台 \|")
EN_TABLE_RE = re.compile(r"\| Tests \| (\d+) tests · cargo-llvm-cov · 3-platform CI \|")

RESULT_RE = re.compile(r"test result: (ok|FAILED)\. (\d+) passed;")


def parse_log(text):
    """从 cargo test 输出中累加 passed 数，并拒绝任何失败运行。"""
    matches = [RESULT_RE.match(line) for line in text.splitlines()]
    matches = [m for m in matches if m]
    if not matches:
        raise SystemExit("错误：日志中找不到 `test result:` 行，请确认传入了完整的 cargo test 输出")
    passed = 0
    failed_runs = 0
    for m in matches:
        if m.group(1) == "FAILED":
            failed_runs += 1
        passed += int(m.group(2))
    if failed_runs:
        raise SystemExit(f"错误：有 {failed_runs} 个测试运行失败，拒绝同步 README 数字")
    return passed


def run_cargo_tests():
    """运行 cargo test --all，返回 passed 总数；测试失败则退出。"""
    proc = subprocess.run(["cargo", "test", "--all"], cwd=REPO, text=True, capture_output=True)
    if proc.returncode != 0:
        sys.stderr.write(proc.stdout)
        sys.stderr.write(proc.stderr)
        raise SystemExit(f"cargo test --all 退出码 {proc.returncode}，拒绝同步 README 数字")
    return parse_log(proc.stdout + "\n" + proc.stderr)


def check_count(passed):
    """校验三处 README 数字是否与 passed 总数一致，返回问题列表。"""
    problems = []
    for path, label, pattern in (
        (README, "README.md 徽章", BADGE_RE),
        (README, "README.md 表格", CN_TABLE_RE),
        (README_EN, "README_EN.md 表格", EN_TABLE_RE),
    ):
        m = pattern.search(path.read_text())
        if m is None:
            problems.append(f"{label}：未找到待校验的模式")
        elif int(m.group(1)) != passed:
            problems.append(f"{label}：当前 {m.group(1)}，应为 {passed}")
    return problems


def update_count(passed, dry_run):
    """把三处 README 数字更新为 passed 总数；无变化时保持原样。"""
    readme = README.read_text()
    readme_en = README_EN.read_text()

    def badge(_m):
        return f"tests-{passed}-brightgreen.svg"

    def cn_table(_m):
        return f"| 测试 | {passed} tests · cargo-llvm-cov · CI 三平台 |"

    def en_table(_m):
        return f"| Tests | {passed} tests · cargo-llvm-cov · 3-platform CI |"

    new_readme = BADGE_RE.sub(badge, readme)
    new_readme = CN_TABLE_RE.sub(cn_table, new_readme)
    new_readme_en = EN_TABLE_RE.sub(en_table, readme_en)

    changed = []
    if new_readme != readme:
        changed.append("README.md")
    if new_readme_en != readme_en:
        changed.append("README_EN.md")
    if not changed:
        print(f"已是最新：passed={passed}，无需修改")
        return
    for name in changed:
        print(f"{name}：测试数已更新为 {passed}")
    if dry_run:
        print("（--dry-run，未写入）")
        return
    README.write_text(new_readme)
    README_EN.write_text(new_readme_en)


def main():
    parser = argparse.ArgumentParser(description="同步/校验 README 测试数")
    parser.add_argument("--check", action="store_true", help="只校验，不写入")
    parser.add_argument("--dry-run", action="store_true", help="显示将要写入的变更，不落盘")
    parser.add_argument("--log", metavar="FILE", help="复用已有 cargo test 日志")
    args = parser.parse_args()

    if args.log:
        passed = parse_log(Path(args.log).read_text())
        print(f"cargo test --all passed 总数：{passed}")
    elif platform.system() != "Linux":
        print(
            "README 测试数的权威口径为 Linux CI 的 cargo test --all passed 总数；"
            f"当前平台为 {platform.system()}，本地运行结果可能不同"
            "（deepseeknova-sandbox 含 Linux 专属测试）。"
        )
        if args.check:
            print("已跳过本地比对；请在 Linux 上运行，或传入 CI 测试日志 --log FILE。")
            return
        raise SystemExit(
            "拒绝在非 Linux 平台覆盖 README 测试数；请改用 CI 测试日志："
            "scripts/sync-test-count.py --log FILE"
        )
    else:
        passed = run_cargo_tests()
        print(f"cargo test --all passed 总数：{passed}")

    if args.check:
        problems = check_count(passed)
        if problems:
            for problem in problems:
                print(f"错误：{problem}")
            raise SystemExit(1)
        print("README 测试数与 passed 总数一致。")
        return
    update_count(passed, args.dry_run)


if __name__ == "__main__":
    main()
