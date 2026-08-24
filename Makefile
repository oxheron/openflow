.PHONY: check fmt test-layout rust integration-workers native desktop release-lint licenses installers arch-bundle

check: test-layout rust native desktop

test-layout:
	./scripts/verify-test-layout.sh

fmt:
	cargo fmt --all --check
	npm --prefix apps/desktop run format:check

rust:
	cargo test --locked --workspace --all-targets
	cargo clippy --locked --workspace --all-targets -- -D warnings

integration-workers:
	cmake -S native -B native/build -DOPENFLOW_BUILD_TESTS=ON
	cmake --build native/build --target openflow-asr-worker openflow-llm-worker --parallel 2
	OPENFLOW_TEST_ASR_WORKER="$$PWD/native/build/openflow-asr-worker" OPENFLOW_TEST_LLM_WORKER="$$PWD/native/build/openflow-llm-worker" cargo test --locked -p openflow-server --all-targets

native:
	cmake -S native -B native/build -DOPENFLOW_BUILD_TESTS=ON
	cmake --build native/build --parallel 2
	ctest --test-dir native/build --output-on-failure

desktop:
	npm --prefix apps/desktop test
	npm --prefix apps/desktop run build

release-lint:
	./scripts/lint-release.sh

licenses:
	npm --prefix apps/desktop ci --ignore-scripts
	./scripts/fetch-inference-sources.sh
	./scripts/generate-third-party-licenses.mjs --write
	./scripts/generate-third-party-licenses.mjs --check

installers:
	./scripts/build-installers.sh

arch-bundle:
	./scripts/build-arch-bundle.sh --profile rocm
