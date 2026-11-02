.PHONY: fmt fmt-check qualification-test study-p0-check study-p0-native-check clippy test check

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

qualification-test:
	PYTHONPATH=scripts python3 -m unittest scripts/test_scope_closure_qualification.py scripts/test_ci_native_fingerprint.py scripts/test_validate_lcia_targets.py

study-p0-check:
	python3 scripts/check_study_p0_benchmarks.py
	PYTHONPATH=scripts python3 -m unittest scripts/test_study_p0_benchmarks.py
	python3 scripts/check_study_p0_cement.py
	PYTHONPATH=scripts python3 -m unittest scripts/test_study_p0_cement.py

study-p0-native-check: study-p0-check
	mkdir -p reports/study-p0
	STUDY_P0_NATIVE_OUTPUT="$(CURDIR)/reports/study-p0/native.json" cargo test -p solver-worker --test study_p0_benchmarks
	python3 scripts/check_study_p0_benchmarks.py --native-results reports/study-p0/native.json

clippy:
	cargo clippy --workspace --all-targets --all-features -- -D warnings

test:
	cargo test --workspace --all-features

check: fmt-check qualification-test study-p0-check clippy test
