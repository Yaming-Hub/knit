# Dataset Training Plan

Track knit's learn→generate round-trip fidelity across 100 public datasets.

## Workflow per dataset

1. Create branch `dataset/{name}`, add `datasets/{name}/` folder
2. Download source data
3. `knit learn` → blueprint
4. `knit generate` → synthetic data
5. Compare original vs synthetic (schema, distributions, stats)
6. Fix knit source code if issues found; iterate steps 3–5
7. Create PR, review with 2 AI models, address comments, merge
8. Record findings in `datasets/{name}/report.md`

## Status Legend

| Symbol | Meaning |
|--------|---------|
| ✅ | Not started |
| 🔄 | In progress |
| ✅ | Complete |
| ❌ | Failed (dataset incompatible or blocked) |

---

## Datasets

### CSV (1–60)

| # | Name | Source | URL | Status | Findings |
|---|------|--------|-----|--------|----------|
| 1 | airports | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/airports.csv` | ✅ | |
| 2 | co2-concentration | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/co2-concentration.csv` | ✅ | |
| 3 | disasters | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/disasters.csv` | ✅ | |
| 4 | flights-airport | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/flights-airport.csv` | ✅ | |
| 5 | gapminder-health-income | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/gapminder-health-income.csv` | ✅ | |
| 6 | github | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/github.csv` | ✅ | |
| 7 | global-temp | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/global-temp.csv` | ✅ | |
| 8 | iowa-electricity | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/iowa-electricity.csv` | ✅ | |
| 9 | la-riots | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/la-riots.csv` | ✅ | |
| 10 | population-engineers-hurricanes | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/population_engineers_hurricanes.csv` | ✅ | |
| 11 | seattle-weather-hourly | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/seattle-weather-hourly-normals.csv` | ✅ | |
| 12 | seattle-weather | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/seattle-weather.csv` | ✅ | |
| 13 | sp500-2000 | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/sp500-2000.csv` | ✅ | |
| 14 | sp500 | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/sp500.csv` | ✅ | |
| 15 | stocks | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/stocks.csv` | ✅ | |
| 16 | species | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/species.csv` | ✅ | |
| 17 | airline-safety | fivethirtyeight | `https://raw.githubusercontent.com/fivethirtyeight/data/master/airline-safety/airline-safety.csv` | ✅ | |
| 18 | bad-drivers | fivethirtyeight | `https://raw.githubusercontent.com/fivethirtyeight/data/master/bad-drivers/bad-drivers.csv` | ✅ | |
| 19 | candy-power-ranking | fivethirtyeight | `https://raw.githubusercontent.com/fivethirtyeight/data/master/candy-power-ranking/candy-data.csv` | ✅ | |
| 20 | drinks | fivethirtyeight | `https://raw.githubusercontent.com/fivethirtyeight/data/master/alcohol-consumption/drinks.csv` | ✅ | |
| 21 | avengers | fivethirtyeight | `https://raw.githubusercontent.com/fivethirtyeight/data/master/avengers/avengers.csv` | ✅ | |
| 22 | us-births-ssa | fivethirtyeight | `https://raw.githubusercontent.com/fivethirtyeight/data/master/births/US_births_2000-2014_SSA.csv` | ✅ | |
| 23 | us-births-cdc | fivethirtyeight | `https://raw.githubusercontent.com/fivethirtyeight/data/master/births/US_births_1994-2003_CDC_NCHS.csv` | ✅ | |
| 24 | bob-ross | fivethirtyeight | `https://raw.githubusercontent.com/fivethirtyeight/data/master/bob-ross/elements-by-episode.csv` | ✅ | |
| 25 | college-majors-all | fivethirtyeight | `https://raw.githubusercontent.com/fivethirtyeight/data/master/college-majors/all-ages.csv` | ✅ | |
| 26 | recent-grads | fivethirtyeight | `https://raw.githubusercontent.com/fivethirtyeight/data/master/college-majors/recent-grads.csv` | ✅ | |
| 27 | women-stem | fivethirtyeight | `https://raw.githubusercontent.com/fivethirtyeight/data/master/college-majors/women-stem.csv` | ✅ | |
| 28 | grad-students | fivethirtyeight | `https://raw.githubusercontent.com/fivethirtyeight/data/master/college-majors/grad-students.csv` | ✅ | |
| 29 | biopics | fivethirtyeight | `https://raw.githubusercontent.com/fivethirtyeight/data/master/biopics/biopics.csv` | ✅ | |
| 30 | dc-characters | fivethirtyeight | `https://raw.githubusercontent.com/fivethirtyeight/data/master/comic-characters/dc-wikia-data.csv` | ✅ | |
| 31 | marvel-characters | fivethirtyeight | `https://raw.githubusercontent.com/fivethirtyeight/data/master/comic-characters/marvel-wikia-data.csv` | ✅ | |
| 32 | nba-raptor | fivethirtyeight | `https://raw.githubusercontent.com/fivethirtyeight/data/master/nba-raptor/modern_RAPTOR_by_player.csv` | ✅ | |
| 33 | steak-survey | fivethirtyeight | `https://raw.githubusercontent.com/fivethirtyeight/data/master/steak-survey/steak-risk-survey.csv` | ✅ | |
| 34 | covid-icu-beds | fivethirtyeight | `https://raw.githubusercontent.com/fivethirtyeight/data/master/covid-geography/mmsa-icu-beds.csv` | ✅ | |
| 35 | alcohol-by-country | plotly | `https://raw.githubusercontent.com/plotly/datasets/master/2010_alcohol_consumption_by_country.csv` | ✅ | |
| 36 | aa-flight-paths | plotly | `https://raw.githubusercontent.com/plotly/datasets/master/2011_february_aa_flight_paths.csv` | ✅ | |
| 37 | us-airport-traffic | plotly | `https://raw.githubusercontent.com/plotly/datasets/master/2011_february_us_airport_traffic.csv` | ✅ | |
| 38 | us-ag-exports | plotly | `https://raw.githubusercontent.com/plotly/datasets/master/2011_us_ag_exports.csv` | ✅ | |
| 39 | apple-stock-2014 | plotly | `https://raw.githubusercontent.com/plotly/datasets/master/2014_apple_stock.csv` | ✅ | |
| 40 | ebola-2014 | plotly | `https://raw.githubusercontent.com/plotly/datasets/master/2014_ebola.csv` | ✅ | |
| 41 | us-cities | plotly | `https://raw.githubusercontent.com/plotly/datasets/master/2014_us_cities.csv` | ✅ | |
| 42 | attention | seaborn | `https://raw.githubusercontent.com/mwaskom/seaborn-data/master/attention.csv` | ✅ | |
| 43 | car-crashes | seaborn | `https://raw.githubusercontent.com/mwaskom/seaborn-data/master/car_crashes.csv` | ✅ | |
| 44 | diamonds | seaborn | `https://raw.githubusercontent.com/mwaskom/seaborn-data/master/diamonds.csv` | ✅ | |
| 45 | dots | seaborn | `https://raw.githubusercontent.com/mwaskom/seaborn-data/master/dots.csv` | ✅ | |
| 46 | dowjones | seaborn | `https://raw.githubusercontent.com/mwaskom/seaborn-data/master/dowjones.csv` | ✅ | |
| 47 | exercise | seaborn | `https://raw.githubusercontent.com/mwaskom/seaborn-data/master/exercise.csv` | ✅ | |
| 48 | flights | seaborn | `https://raw.githubusercontent.com/mwaskom/seaborn-data/master/flights.csv` | ✅ | |
| 49 | fmri | seaborn | `https://raw.githubusercontent.com/mwaskom/seaborn-data/master/fmri.csv` | ✅ | |
| 50 | geyser | seaborn | `https://raw.githubusercontent.com/mwaskom/seaborn-data/master/geyser.csv` | ✅ | |
| 51 | glue | seaborn | `https://raw.githubusercontent.com/mwaskom/seaborn-data/master/glue.csv` | ✅ | |
| 52 | healthexp | seaborn | `https://raw.githubusercontent.com/mwaskom/seaborn-data/master/healthexp.csv` | ✅ | |
| 53 | iris | seaborn | `https://raw.githubusercontent.com/mwaskom/seaborn-data/master/iris.csv` | ✅ | |
| 54 | mpg | seaborn | `https://raw.githubusercontent.com/mwaskom/seaborn-data/master/mpg.csv` | ✅ | |
| 55 | penguins | seaborn | `https://raw.githubusercontent.com/mwaskom/seaborn-data/master/penguins.csv` | ✅ | |
| 56 | planets | seaborn | `https://raw.githubusercontent.com/mwaskom/seaborn-data/master/planets.csv` | ✅ | |
| 57 | seaice | seaborn | `https://raw.githubusercontent.com/mwaskom/seaborn-data/master/seaice.csv` | ✅ | |
| 58 | tips | seaborn | `https://raw.githubusercontent.com/mwaskom/seaborn-data/master/tips.csv` | ✅ | |
| 59 | titanic | seaborn | `https://raw.githubusercontent.com/mwaskom/seaborn-data/master/titanic.csv` | ✅ | |
| 60 | taxis | seaborn | `https://raw.githubusercontent.com/mwaskom/seaborn-data/master/taxis.csv` | ✅ | |

### JSON (61–80)

| # | Name | Source | URL | Status | Findings |
|---|------|--------|-----|--------|----------|
| 61 | anscombe | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/anscombe.json` | ✅ | |
| 62 | barley | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/barley.json` | ✅ | |
| 63 | burtin | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/burtin.json` | ✅ | |
| 64 | cars-json | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/cars.json` | ✅ | |
| 65 | countries | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/countries.json` | ✅ | |
| 66 | crimea | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/crimea.json` | ✅ | |
| 67 | driving | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/driving.json` | ✅ | |
| 68 | flare | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/flare.json` | ✅ | |
| 69 | gapminder | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/gapminder.json` | ✅ | |
| 70 | income | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/income.json` | ✅ | |
| 71 | london-centroids | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/londonCentroids.json` | ✅ | |
| 72 | miserables | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/miserables.json` | ✅ | |
| 73 | monarchs | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/monarchs.json` | ✅ | |
| 74 | movies | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/movies.json` | ❌ | |
| 75 | normal-2d | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/normal-2d.json` | ✅ | |
| 76 | obesity | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/obesity.json` | ✅ | |
| 77 | ohlc | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/ohlc.json` | ✅ | |
| 78 | penguins-json | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/penguins.json` | ✅ | |
| 79 | population | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/population.json` | ✅ | |
| 80 | unemployment-industries | vega-datasets | `https://raw.githubusercontent.com/vega/vega-datasets/main/data/unemployment-across-industries.json` | ✅ | |

### JSONL (81–90)

| # | Name | Source | URL | Status | Findings |
|---|------|--------|-----|--------|----------|
| 81 | piqa-valid | PIQA | `https://raw.githubusercontent.com/ybisk/ybisk.github.io/master/piqa/data/valid.jsonl` | ✅ | |
| 82 | piqa-test | PIQA | `https://raw.githubusercontent.com/ybisk/ybisk.github.io/master/piqa/data/tests.jsonl` | ✅ | |
| 83 | piqa-train | PIQA | `https://raw.githubusercontent.com/ybisk/ybisk.github.io/master/piqa/data/train.jsonl` | ✅ | |
| 84 | nq-open-dev | NQ-Open | `https://raw.githubusercontent.com/google-research-datasets/natural-questions/master/nq_open/NQ-open.dev.jsonl` | ✅ | |
| 85 | nq-open-train | NQ-Open | `https://raw.githubusercontent.com/google-research-datasets/natural-questions/master/nq_open/NQ-open.train.jsonl` | ✅ | |
| 86 | nq-efficientqa-dev | NQ-Open | `https://raw.githubusercontent.com/google-research-datasets/natural-questions/master/nq_open/NQ-open.efficientqa.dev.1.1.jsonl` | ✅ | |
| 87 | nq-efficientqa-dev-noann | NQ-Open | `https://raw.githubusercontent.com/google-research-datasets/natural-questions/master/nq_open/NQ-open.efficientqa.dev.1.1.no-annotations.jsonl` | ✅ | |
| 88 | nq-efficientqa-test | NQ-Open | `https://raw.githubusercontent.com/google-research-datasets/natural-questions/master/nq_open/NQ-open.efficientqa.test.1.1.jsonl` | ✅ | |
| 89 | nq-efficientqa-test-noann | NQ-Open | `https://raw.githubusercontent.com/google-research-datasets/natural-questions/master/nq_open/NQ-open.efficientqa.test.1.1.no-annotations.jsonl` | ✅ | |
| 90 | nq-efficientqa-sample | NQ-Open | `https://raw.githubusercontent.com/google-research-datasets/natural-questions/master/nq_open/NQ-open.efficientqa.dev.1.1.sample.jsonl` | ✅ | |

### Parquet (91–100)

| # | Name | Source | URL | Status | Findings |
|---|------|--------|-----|--------|----------|
| 91 | arc-easy-valid | ai2_arc | `https://huggingface.co/api/datasets/allenai/ai2_arc/parquet/ARC-Easy/validation/0.parquet` | ✅ | |
| 92 | arc-easy-train | ai2_arc | `https://huggingface.co/api/datasets/allenai/ai2_arc/parquet/ARC-Easy/train/0.parquet` | ✅ | |
| 93 | arc-challenge-valid | ai2_arc | `https://huggingface.co/api/datasets/allenai/ai2_arc/parquet/ARC-Challenge/validation/0.parquet` | ✅ | |
| 94 | arc-challenge-train | ai2_arc | `https://huggingface.co/api/datasets/allenai/ai2_arc/parquet/ARC-Challenge/train/0.parquet` | ✅ | |
| 95 | glue-sst2 | GLUE | `https://huggingface.co/api/datasets/nyu-mll/glue/parquet/sst2/validation/0.parquet` | ✅ | |
| 96 | glue-cola | GLUE | `https://huggingface.co/api/datasets/nyu-mll/glue/parquet/cola/validation/0.parquet` | ✅ | |
| 97 | glue-mrpc | GLUE | `https://huggingface.co/api/datasets/nyu-mll/glue/parquet/mrpc/validation/0.parquet` | ✅ | |
| 98 | glue-rte | GLUE | `https://huggingface.co/api/datasets/nyu-mll/glue/parquet/rte/validation/0.parquet` | ✅ | |
| 99 | glue-wnli | GLUE | `https://huggingface.co/api/datasets/nyu-mll/glue/parquet/wnli/validation/0.parquet` | ✅ | |
| 100 | glue-stsb | GLUE | `https://huggingface.co/api/datasets/nyu-mll/glue/parquet/stsb/validation/0.parquet` | ✅ | |

---

## Global Findings

Issues discovered across multiple datasets and code fixes applied:

| PR | Datasets affected | Issue | Fix |
|----|-------------------|-------|-----|
| #333 | co2-concentration, all date columns | Faker "date"/"datetime" generators rejected by validator on Date/Datetime data_type | Updated `check_generator_type_compat` to allow temporal faker methods on temporal types |
| #333 | covid-icu-beds, string columns with NA | String-source columns with numeric content got distribution generators (type mismatch) | Changed `build_generator_inner` to skip distributions entirely for string-source columns |
| - | biopics, avengers | Latin-1 encoded CSVs fail Arrow UTF-8 reader | Converted to UTF-8 before ingestion (knit requires UTF-8) |
| - | movies | JSON field "Title" has mixed types (string + number 1776) | Cannot fix — Arrow JSON reader limitation. Marked as incompatible |

---

## Progress Summary

- **Completed**: 99 / 100
- **Failed**: 1 (movies — mixed JSON types)
- **Code fixes**: 2 (faker/date validator, string-source distribution)

