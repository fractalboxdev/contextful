# Point-in-time evaluation

Evaluating a reader, an agent or a forecast as of a past instant without letting
later information in. Leaks enter through five layers: the data (backfill stamps,
publication lag, restatements), retrieval (live search, rankers trained on later
data), the model's weights, the tools around it, and the evaluation protocol
(overlapping labels, selection over many trials). The controls these sources bear
on are an availability-time column beside the two clocks of `store.bound-time`,
vintages, a per-row as-of join, a read horizon, walk-forward sweeps and a backtest
protocol with a trial ledger.

## Domain-neutral controls and consumer profiles

`topology.bound-application.domain-neutral` keeps source availability, label intervals,
checkpoint cutoffs, bounded reads and trajectory replay independent of an industry.
Backtesting means historical evaluation; forecasting means probability estimates for
consumer-declared outcomes. Neither term selects financial data.

Financial return-series profiles use `assurance.backtest.trial-ledger` for deflated
Sharpe scoring, `assurance.backtest.fill-instant` for price timing, and
`assurance.backtest.planted-delisting` for delisting-return coverage. The consumer's
execution component supplies costs, slippage, sizing and liquidity assumptions through
the opaque parameters recorded by `assurance.backtest.execution-component`.
Prediction-market profiles use probability prices under
`assurance.forecast.market-context` and `assurance.forecast.imputed`. These named
profiles give the financial literature below a specific application boundary; general
cases carry source records, declared outcomes and execution metadata.
Return-series consumers call `TrialLedger`; case `delisting` and forecast `market_price`
select the corresponding optional paths.

## Model memory

### FinCAD

Li, W. W., Wang, M., Ma, T. "Summoning the Oracle to Slay It: Mitigating Look-Ahead
Bias in Financial Backtesting with Large Language Models." *EMNLP*, 2026.
<https://arxiv.org/abs/2605.24564>

- **Priority:** should-read
- **Informs:** `connector.infer`, `assurance.evaluate`
- **Question:** Can memorized outcomes be attenuated at decoding time without
  retraining? Context-aware decoding scaled per entity and date gives a largest
  model-level mean in-sample return correction of −67.1% and lifts the
  in-sample/out-of-sample rank correlation from 0.779 to 0.846; it needs logits,
  which the single model endpoint does not promise.

### Lookahead Propensity

Gao, Z., Jiang, W., Yan, Y. "Detecting Lookahead Bias in LLM Forecasts."
arXiv:2512.23847, 2025. <https://arxiv.org/abs/2512.23847>

- **Priority:** must-read
- **Informs:** `connector.infer`, `assurance.evaluate`, `read.settle`
- **Question:** How is leakage detected per case rather than per period? A date-only
  recall query estimates, per firm-date pair, the probability the model holds the
  outcome; it collapses to about zero right after the cutoff, and a positive
  interaction with forecast accuracy flags contamination. It needs first-token
  probabilities from the endpoint.

### Temporal leakage in LLM backtesting

Zhang, Z., Stadie, B. C. "Temporal Leakage in LLM Backtesting: Measurement,
Validation, and Adjusted Scores." arXiv:2608.02985, 2026.
<https://arxiv.org/abs/2608.02985>

- **Priority:** must-read
- **Informs:** `assurance.evaluate`, `read.settle`
- **Question:** Is a before/after-cutoff score split a leakage test? No: four flagship
  models fail it on questions that all resolved after their cutoffs, because recency
  mimics leakage. Measurement needs a reference — a known cutoff or a matched clean
  control — and keys each case on its resolution instant.

### The memorization problem

Lopez-Lira, A., Tang, Y., Zhu, M. "The Memorization Problem: Can We Trust LLMs'
Economic Forecasts?" arXiv:2504.14765, 2025. <https://arxiv.org/abs/2504.14765>

- **Priority:** must-read
- **Informs:** `connector.infer`, `read.rank`, `assurance.evaluate`
- **Question:** Does an instruction to respect a historical boundary bound the model?
  No: models recall exact pre-cutoff economic values despite the instruction,
  reconstruct masked entities and dates from minimal context, and embeddings memorize
  too — so a ranked read over an embedder trained after the horizon also leaks.

### Logit-guided unlearning

Merchant, H., Levy, B. "A Fast and Effective Solution to the Problem of Look-ahead
Bias in LLMs." arXiv:2512.06607, 2025. <https://arxiv.org/abs/2512.06607>

- **Priority:** optional
- **Informs:** `connector.infer`
- **Question:** What does inference-time removal cost compared with retraining? Two
  small models, one tuned on facts to forget and one on facts to keep, adjust a large
  base model's logits; it removes verbatim and semantic knowledge, but again requires
  logit access at the endpoint.

### Anonymization and information loss

Wu, K., Yang, B., Ying, Z., Zhou, D. "Anonymization and Information Loss."
arXiv:2511.15364, 2025. <https://arxiv.org/abs/2511.15364>

- **Priority:** should-read
- **Informs:** `assurance.evaluate`
- **Question:** Does the gap between raw and anonymized inputs measure leakage? No:
  on S&P credit downgrades raw text wins for reasons other than look-ahead bias,
  because masking removes informative numbers and entities; the "anonymization gap"
  quantifies that loss without outcome data.

### Look-ahead bias in GPT sentiment

Glasserman, P., Lin, C. "Assessing Look-Ahead Bias in Stock Return Predictions
Generated By GPT Sentiment Analysis." arXiv:2309.17322, 2023.
<https://arxiv.org/abs/2309.17322>

- **Priority:** should-read
- **Informs:** `assurance.evaluate`
- **Question:** What else moves a with/without-identity comparison? A distraction
  effect: in-sample, anonymized headlines outperform originals, most for large firms,
  so a paired-context gap is a diagnostic, not a leakage measure.

### DatedGPT

Yan, Y., Tang, R., Gao, Z., Jiang, W., Lu, Y. "DatedGPT: Preventing Lookahead Bias in
Large Language Models with Time-Aware Pretraining." arXiv:2603.11838, 2026.
<https://arxiv.org/abs/2603.11838>

- **Priority:** must-read
- **Informs:** `connector.infer`, `assurance.evaluate`
- **Question:** How large is the leakage premium, and where does post-training add
  knowledge? Twelve 1.3B models with annual cutoffs 2013–2024 and year-bounded
  instruction data give a 3.20 Sharpe without look-ahead; models trained over the
  outcome period add 26.4 bp per standard deviation. A model record needs separate
  pre-training and post-training cutoffs.

### Point-in-time language models at scale

Kelly, B., Malamud, S., Schwab, J., Xu, T. A. "Scaling Point-in-Time Language Models:
Enabling Economic Evaluation of Embeddings." arXiv:2607.11889, 2026.
<https://arxiv.org/abs/2607.11889>

- **Priority:** should-read
- **Informs:** `connector.infer`, `read.rank`
- **Question:** Do point-in-time checkpoints exist that a reader or embedder can be
  routed to by horizon? Monthly checkpoints 2013–2024, up to 4B parameters on 1T
  chronologically filtered tokens, approach unrestricted models of the same size.

### ChronoBERT and ChronoGPT

He, S., Lv, L., Manela, A., Wu, J. "Chronologically Consistent Large Language
Models." arXiv:2502.21206, 2025. <https://arxiv.org/abs/2502.21206>

- **Priority:** should-read
- **Informs:** `connector.infer`, `read.rank`
- **Question:** Does a date-bounded encoder give up the signal? On next-day returns
  from news, ChronoBERT and ChronoGPT reach Sharpe ratios comparable to a much larger
  Llama model, so the measured look-ahead bias there is modest.

### Dated Data

Cheng, J., Marone, M., Weller, O., Lawrie, D., Khashabi, D., Van Durme, B. "Dated
Data: Tracing Knowledge Cutoffs in Large Language Models." arXiv:2403.12958, 2024.
<https://arxiv.org/abs/2403.12958>

- **Priority:** must-read
- **Informs:** `connector.infer`, `assurance.evaluate`
- **Question:** Is a reported cutoff the effective one? No: effective cutoffs differ
  per resource and from the reported date, driven by old pages in new CommonCrawl
  dumps and deduplication, so a cutoff is a measured interval and an evaluation
  sweeps where it sits.

### Current-year probe

van Adrichem, S., Bhaskar, A., Yang, D., Potts, C., Huang, J. "Do Language Models
Consistently Encode the Current Year?" *COLM*, 2026.
<https://arxiv.org/abs/2608.15507>

- **Priority:** optional
- **Informs:** `connector.infer`
- **Question:** How is an undocumented cutoff estimated? A verb-tense task on base
  models tracks the pre-training cutoff within 10 months on average across 13 models;
  prompting shifts the declared year (94.6%) but not the associative one (1.7%), so a
  prompt never moves what the weights know.

## Retrieval leakage and forecasting

### Date-filtered web retrieval

El Lahib, A., Xia, Y.-J., Li, Z., Wang, Y., Pi, X. "Temporal Leakage in
Search-Engine Date-Filtered Web Retrieval: A Retrospective Forecasting Case Study."
*ACL*, 2026. <https://arxiv.org/abs/2602.00758>

- **Priority:** must-read
- **Informs:** `read.retrieve`, `connector.source`, `assurance.evaluate`
- **Question:** Is a search engine's date filter a control? No: at least one
  retrieved page leaks post-cutoff content for 71% (Google) and 81% (DuckDuckGo) of
  questions, the answer is revealed outright for 41% and 55%, and Brier falls from
  0.24 clean to 0.10 leaky. Frozen, time-stamped snapshots replace live search.

### Pitfalls in evaluating forecasters

Paleka, D., Goel, S., Geiping, J., Tramèr, F. "Pitfalls in Evaluating Language Model
Forecasters." arXiv:2506.00723, 2025. <https://arxiv.org/abs/2506.00723>

- **Priority:** must-read
- **Informs:** `read.retrieve`, `read.rank`, `read.settle`, `assurance.evaluate`
- **Question:** Which temporal leaks survive a date filter? Three families: question
  selection made knowing the resolution, date-restricted retrieval (wrong dates, page
  modules written later, retrievers and rankers trained on later data), and stated
  cutoffs treated as guarantees. A ranker without learned knowledge, such as TF-IDF,
  avoids the retriever leak at a cost in quality.

### Bench to the Future

Wildman, J., Bosse, N. I., Hnyk, D., Mühlbacher, P., Hambly, F., Evans, J.,
Schwarz, D., Phillips, L. "Bench to the Future: A Pastcasting Benchmark for
Forecasting Agents." arXiv:2506.21558, 2025. <https://arxiv.org/abs/2506.21558>

- **Priority:** should-read
- **Informs:** `read.retrieve`, `read.settle`, `assurance.evaluate`
- **Question:** Can resolved questions be forecast hermetically? Hundreds of resolved
  questions, each with an offline corpus of tens of thousands of pages, produce
  results comparable to live forecasting on unresolved questions.

### Deep Research Bench and RetroSearch

Bosse, N. I., Evans, J., Gambee, R. G., Hnyk, D., Mühlbacher, P., Phillips, L.,
Schwarz, D., Wildman, J. "Deep Research Bench: Evaluating AI Web Research Agents."
arXiv:2506.06287, 2025. <https://arxiv.org/abs/2506.06287>

- **Priority:** should-read
- **Informs:** `read.retrieve`, `connector.source`, `assurance.evaluate`
- **Question:** Does a frozen web snapshot lose fidelity against live search? Over 89
  multi-step research tasks, agents on the frozen RetroSearch corpus perform
  comparably to live-web agents, which supports a snapshot corpus captured before the
  horizon as a store object.

### Approaching human-level forecasting

Halawi, D., Zhang, F., Chen, Y.-H., Steinhardt, J. "Approaching Human-Level
Forecasting with Language Models." arXiv:2402.18563, 2024.
<https://arxiv.org/abs/2402.18563>

- **Priority:** should-read
- **Informs:** `read.retrieve`, `read.settle`
- **Question:** What baseline does a retrieval-augmented forecaster set? Scored only
  on questions published after the models' cutoffs, the system nears the crowd
  aggregate of competitive forecasters and in some settings surpasses it.

### ForecastBench

Karger, E., Bastani, H., Chen, Y.-H., Jacobs, Z., Halawi, D., Zhang, F.,
Tetlock, P. E. "ForecastBench: A Dynamic Benchmark of AI Forecasting Capabilities."
arXiv:2409.19839, 2024. <https://arxiv.org/abs/2409.19839>; leaderboards at
<https://www.forecastbench.org/leaderboards/>

- **Priority:** must-read
- **Informs:** `read.settle`, `assurance.evaluate`
- **Question:** How does a benchmark avoid leakage entirely? Only questions with no
  known answer at submission, 1,000 regenerated over time; expert forecasters beat
  the best LLM (p < 0.001, N = 200). The leaderboards score a difficulty-adjusted
  Brier index and admit a model 50 days after submission.

### FutureX

Zeng, Z., Liu, J., Chen, S., et al. "FutureX: An Advanced Live Benchmark for LLM
Agents in Future Prediction." arXiv:2508.11987, 2025.
<https://arxiv.org/abs/2508.11987>

- **Priority:** optional
- **Informs:** `read.settle`, `assurance.evaluate`
- **Question:** What fails when agents search for future events? Across 25 models,
  daily-updated questions expose vulnerability to fake web pages and to stale
  temporal validity in retrieved content.

### PROPHET

Tao, Z., Wu, P., Jin, Z., et al. "PROPHET: An Inferable Future Forecasting Benchmark
with Causal Intervened Likelihood Estimation." arXiv:2504.01509, 2025.
<https://arxiv.org/abs/2504.01509>

- **Priority:** optional
- **Informs:** `read.retrieve`, `assurance.evaluate`
- **Question:** Is every question answerable from the corpus provided? Not by
  default; a causal-intervened likelihood filters out questions the paired news
  cannot support, separating corpus gaps from reader failures.

### Prophet Arena

Yang, Q., Mahns, S., Li, S., Gu, A., Wu, J., Xu, H. "LLM-as-a-Prophet: Understanding
Predictive Intelligence with Prophet Arena." arXiv:2510.17638, 2025.
<https://arxiv.org/abs/2510.17638>

- **Priority:** optional
- **Informs:** `read.settle`, `assurance.evaluate`
- **Question:** Where does a live forecaster lose to the market? Calibration is small,
  but event recall is inaccurate and information aggregation lags markets as
  resolution nears, which argues for a minimum lead time between forecast and
  resolution.

### KalshiBench

Nel, L. "Do Large Language Models Know What They Don't Know? Kalshibench: A New
Benchmark for Evaluating Epistemic Calibration via Prediction Markets."
arXiv:2512.16030, 2025. <https://arxiv.org/abs/2512.16030>

- **Priority:** should-read
- **Informs:** `read.settle`
- **Question:** Are post-cutoff forecasts calibrated? Across 300 market questions
  resolving after the cutoffs, all five models are overconfident (best ECE 0.120;
  a reasoning-enhanced model 0.395) and one alone has a positive Brier skill
  score — the measures the calibration report already names.

## Finance benchmarks and backtest method

### FINSABER

Li, W. W., Kim, H., Cucuringu, M., Ma, T. "Can LLM-based Financial Investing
Strategies Outperform the Market in Long Run?" *KDD*, 2026.
<https://arxiv.org/abs/2505.07078>

- **Priority:** must-read
- **Informs:** `assurance.evaluate`, `store.bound-time`, `run.backfill`
- **Question:** Do reported LLM trading advantages survive a broad, long window? No:
  over two decades and 100+ symbols, with survivorship and data snooping controlled,
  the advantages deteriorate; strategies are too conservative in bull markets and too
  aggressive in bear markets.

### StockBench

Chen, Y., Yao, Z., Liu, Y., Xin, A., Ye, J., Yu, J., Hou, L., Li, J. "StockBench: Can
LLM Agents Trade Stocks Profitably In Real-world Markets?" arXiv:2510.02209, 2025.
<https://arxiv.org/abs/2510.02209>

- **Priority:** should-read
- **Informs:** `assurance.evaluate`
- **Question:** Does static financial QA predict trading? No: over multi-month
  windows built to be contamination-free, most agents fail to beat buy-and-hold.

### InvestorBench

Li, H., Cao, Y., Yu, Y., et al. "INVESTORBENCH: A Benchmark for Financial
Decision-Making Tasks with LLM-based Agent." arXiv:2412.18174, 2024.
<https://arxiv.org/abs/2412.18174>

- **Priority:** optional
- **Informs:** `assurance.evaluate`
- **Question:** What task shape do agent backtests share? Stocks, crypto and ETFs
  across 13 backbone models in one environment, with no control for the backbones'
  cutoffs.

### FinBen

Xie, Q., Han, W., Chen, Z., et al. "FinBen: A Holistic Financial Benchmark for Large
Language Models." arXiv:2402.12659, 2024. <https://arxiv.org/abs/2402.12659>

- **Priority:** optional
- **Informs:** `assurance.evaluate`
- **Question:** What does a broad static suite cover? 36 datasets over 24 tasks and
  15 models, including stock trading; its historical windows overlap the models'
  training data.

### LiveTradeBench

Yu, H., Li, F., You, J. "LiveTradeBench: Seeking Real-World Alpha with Large Language
Models." arXiv:2511.03628, 2025. <https://arxiv.org/abs/2511.03628>

- **Priority:** should-read
- **Informs:** `assurance.evaluate`
- **Question:** What does live evaluation buy, and what does it cost? No leakage by
  construction, but 50 days across 21 models is short; leaderboard scores do not
  predict trading results.

### DeepFund

Li, C., Shi, Y., Wang, C., et al. "Time Travel is Cheating: Going Live with DeepFund
for Real-Time Fund Investment Benchmarking." *NeurIPS Datasets and Benchmarks*,
2025.
<https://arxiv.org/abs/2505.11065>

- **Priority:** should-read
- **Informs:** `assurance.evaluate`
- **Question:** What happens once data postdates every cutoff? Nine flagship models
  incur net trading losses on market data published after their pre-training
  cutoffs.

### Agent Market Arena

Qian, L., Peng, X., Wang, Y., et al. "When Agents Trade: Live Multi-Market Trading
Benchmark for LLM Agents." arXiv:2510.11695, 2025.
<https://arxiv.org/abs/2510.11695>

- **Priority:** optional
- **Informs:** `assurance.evaluate`
- **Question:** What drives outcome variance in live agent trading? The agent
  framework more than the model backbone, across crypto and equity markets.

### Look-Ahead-Bench

Benhenda, M. "Look-Ahead-Bench: a Standardized Benchmark of Look-ahead Bias in
Point-in-Time LLMs for Finance." arXiv:2601.13770, 2026.
<https://arxiv.org/abs/2601.13770>

- **Priority:** should-read
- **Informs:** `connector.infer`, `assurance.evaluate`
- **Question:** How is look-ahead measured in a workflow rather than by QA? As alpha
  decay across temporally distinct regimes against quantitative baselines; standard
  Llama and DeepSeek models decay where point-in-time models do not.

### The Deflated Sharpe Ratio

Bailey, D. H., López de Prado, M. "The Deflated Sharpe Ratio: Correcting for
Selection Bias, Backtest Overfitting and Non-Normality." *Journal of Portfolio
Management* 40(5):94–107, 2014. <https://doi.org/10.3905/jpm.2014.40.5.094>

- **Priority:** must-read
- **Informs:** `assurance.evaluate`, `assurance.baseline`, `run.record`
- **Question:** What does selection over many trials do to a reported Sharpe? It
  inflates it; the deflated ratio corrects using the number of trials, the variance
  of Sharpe across them, sample length, skew and kurtosis — inputs a self-reported
  trial count cannot supply, so trials are recorded as run history.

### Purged and combinatorial purged cross-validation

López de Prado, M. *Advances in Financial Machine Learning.* Wiley, 2018, ch. 7
("Cross-Validation in Finance") and ch. 12 ("Backtesting through Cross-Validation").
ISBN 978-1-119-48208-6.

- **Priority:** must-read
- **Informs:** `assurance.evaluate`, `read.settle`
- **Question:** How do overlapping labels leak across a split? Purging drops training
  cases whose label interval overlaps a test label; embargo drops cases right after
  the test span; combinatorial purging yields a distribution of out-of-sample paths.
  Neither works unless each case declares its label interval.

## Data systems

### SQL:2011 periods

Kulkarni, K., Michels, J.-E. "Temporal features in SQL:2011." *SIGMOD Record*
41(3):34–43, 2012. <https://doi.org/10.1145/2380776.2380786>

- **Priority:** should-read
- **Informs:** `store.bound-time`, `store.fold`
- **Question:** How many engine clocks does the standard define? Two: application
  periods over user columns and a system-versioned period the engine stamps. An
  availability time belongs with the user columns, not as a third engine clock.

### Effective timestamping

Torp, K., Jensen, C. S., Snodgrass, R. T. "Effective Timestamping in Databases."
*VLDB Journal* 8(3–4):267–288, 2000. <https://doi.org/10.1007/s007780050008>

- **Priority:** should-read
- **Informs:** `store.bound-time`, `run.backfill`, `run.seed`
- **Question:** When is a timestamp assigned within a transaction, and what if the
  system supplies a default valid time? A defaulted valid time must be handled like
  transaction time or users can extract inconsistent states — the trap of filling an
  availability column from the load instant.

### Datomic as-of

Datomic. "Filters: as-of, since, history."
<https://docs.datomic.com/reference/filters.html>

- **Priority:** optional
- **Informs:** `store.bound-time`, `run.record`
- **Question:** What does an as-of database value bound? Transactions only: it ignores
  every transaction after the point, so on a history loaded today it cannot answer
  what was available at an earlier date.

### XTDB bitemporality

XTDB. "Time in XTDB." <https://docs.xtdb.com/about/time-in-xtdb.html>

- **Priority:** should-read
- **Informs:** `store.bound-time`
- **Question:** Can the engine stamp be overridden to backdate history? Users have no
  control over system time; corrections and future-dated facts go through the
  user-editable valid-time line, and system time defaults to "as best known".

### Feast point-in-time joins

Feast. "Point-in-time joins."
<https://docs.feast.dev/getting-started/concepts/point-in-time-joins>

- **Priority:** must-read
- **Informs:** `store.bound-time`, `read.register`
- **Question:** Which timestamp does a per-row as-of join filter on? By default the
  event timestamp, within a TTL relative to each entity row; the created timestamp
  only breaks ties, so a restated value reaches an earlier row unless
  `filter_by_created_timestamp` is set.

### Tecton data delay

Tecton. "BatchSource — `data_delay`."
<https://docs.tecton.ai/docs/sdk-reference/data-sources/BatchSource>

- **Priority:** optional
- **Informs:** `run.backfill`, `store.bound-time`
- **Question:** How does a feature platform model publication lag? As a declared
  delay after the batch schedule before materialization, so a value is not served
  before its data has arrived.

### kdb+ as-of join

KX. "aj, aj0, ajf, ajf0 — as-of join." <https://code.kx.com/q/ref/aj/>

- **Priority:** should-read
- **Informs:** `read.register`, `store.bound-time`
- **Question:** What is the minimal as-of join? Equality on key columns and, on the
  last column, the most recent value at or before each left row's time — one output
  row per left row.

### ALFRED vintages

Federal Reserve Bank of St. Louis. "ALFRED: Archival FRED."
<https://alfred.stlouisfed.org/help>

- **Priority:** must-read
- **Informs:** `store.bound-time`, `connector.source`, `store.fold`
- **Question:** How are revised series kept answerable as of a past date? Each
  vintage is the series as the source reported it on a date, release dates mark first
  availability, and earlier vintages survive revision — the shape a restated window
  overwrites today.

### Compustat Point-in-Time

S&P Global Market Intelligence. "Compustat Financials."
<https://www.marketplace.spglobal.com/en/datasets/compustat-financials-(8)>

- **Priority:** should-read
- **Informs:** `store.bound-time`, `connector.source`
- **Question:** What does a vendor point-in-time fundamentals feed carry? Every change
  to a fundamental item with its point date, originally reported beside restated
  values, from 1987 for North American companies; scope and start dates differ across
  descriptions and are unsettled.
