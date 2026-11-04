.PHONY: fmt fmt-check qualification-test study-p0-check study-p0-native-check study-method-check study-method-native-check clippy test check

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

study-method-check:
	python3 scripts/check_study_method_assessment.py
	PYTHONPATH=scripts python3 -m unittest scripts/test_study_method_assessment.py
	python3 scripts/generate_study_method_runtime_schemas.py --check
	python3 scripts/check_study_method_runtime.py
	PYTHONPATH=scripts python3 -m unittest scripts/test_study_method_runtime.py

study-method-native-check: study-method-check
	mkdir -p reports/study-method
	STUDY_METHOD_ARITHMETIC_OUTPUT="$(CURDIR)/reports/study-method/arithmetic.json" cargo test -p solver-worker --lib source_ordered_mixed_arithmetic
	python3 scripts/check_study_method_assessment.py --native-results reports/study-method/arithmetic.json

clippy:
	cargo clippy --workspace --all-targets --all-features -- -D warnings

test:
	cargo test --workspace --all-features

check: fmt-check qualification-test study-p0-check study-method-check clippy test
