# Changelog

## 0.1.0 (2026-07-06)


### Features

* **audit:** StaticAuditor with dangerous-pattern table, llm/http stubs, chain builder ([1f5b146](https://github.com/roxblnfk/zed-skills/commit/1f5b146c12dc521902b5bf5836d75482abbba156))
* **cli:** --re-audit flag, config-driven audit chain, [audit] output in update and show ([1555702](https://github.com/roxblnfk/zed-skills/commit/15557029507ab5780ed36e70b3a0d411a956c6ab))
* **cli:** positional package filters, --trust, --discovery; show annotations ([a2834f5](https://github.com/roxblnfk/zed-skills/commit/a2834f519240c0a0c922b2cbcae7e38a0d903ede))
* **cli:** remote provider wiring, --from/--refresh, skills add, cache gitignore ([90f8a0e](https://github.com/roxblnfk/zed-skills/commit/90f8a0e190783f08bbc11ecc8f65d620adcea20a))
* **cli:** skills binary with init, update and show ([329bf2d](https://github.com/roxblnfk/zed-skills/commit/329bf2d3fba5d06ad72a6265e5879c47fd5b1e1b))
* **core,cli:** directory-level aliases (junction/symlink) with state matrix ([63fb002](https://github.com/roxblnfk/zed-skills/commit/63fb002ddf90a6e5df98de14a0a68c700433bb87))
* **core,providers:** offline cache-only materialize mode ([56e24fb](https://github.com/roxblnfk/zed-skills/commit/56e24fb936f079bd0ad6ca2887edae4067a162dc))
* **core:** audit pipeline config, on-fail aggregation, lockfile verdict cache ([24578ae](https://github.com/roxblnfk/zed-skills/commit/24578ae8be13e027ebdb627259c30e3496a6e03f))
* **core:** domain types, manifest, lockfile, frontmatter reader ([60c8265](https://github.com/roxblnfk/zed-skills/commit/60c8265911855bbec127883cb83dd2c350c355ea))
* **core:** trust model, vendor patterns, discovery gating, partial-run planning ([88cc2fa](https://github.com/roxblnfk/zed-skills/commit/88cc2fa71fe784da31b7e9cd6d2ebcf8f920b584))
* **core:** typed pipeline with transactional non-destructive sync ([794e126](https://github.com/roxblnfk/zed-skills/commit/794e126bc239a102400448d917f6fe02e8180c24))
* **core:** url origin, url provider id, refresh-aware cache plumbing ([aaaa137](https://github.com/roxblnfk/zed-skills/commit/aaaa137f348bcca1aa837d6000b0bf439066b291))
* **lsp,cli:** diagnostics-first LSP server, skills lsp subcommand ([33e54c1](https://github.com/roxblnfk/zed-skills/commit/33e54c110154793b65f1e18f75e157d560335226))
* **providers,audit:** DirProvider, DeclaredLocator, contract testkit, noop audit chain ([3762c0d](https://github.com/roxblnfk/zed-skills/commit/3762c0d09c18afed6f6aec1bcc9c3a0b9ff01648))
* **providers:** ComposerProvider, RecursiveFallbackLocator, treescan ([e44a9d2](https://github.com/roxblnfk/zed-skills/commit/e44a9d2afe5d62d149d33883b7cf455da645f8b1))
* **providers:** remote providers, archive cache, ref resolver, locator chain ([23edacf](https://github.com/roxblnfk/zed-skills/commit/23edacfacf921c647e0eeb76b29f9e94d175e72b))


### Bug Fixes

* **core:** keep malformed-donor warn note ASCII-only ([2edecf9](https://github.com/roxblnfk/zed-skills/commit/2edecf94f99dc71c3ed9e69267c0cfef2dd400ba))
