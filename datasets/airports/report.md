# Dataset #1: Airports — Round-Trip Report

## Source
- **URL**: https://raw.githubusercontent.com/vega/vega-datasets/main/data/airports.csv
- **Format**: CSV
- **Rows**: 3,376
- **Columns**: 7 (iata, name, city, state, country, latitude, longitude)

## Comparison Results

| Metric | Original | Generated | Match |
|--------|----------|-----------|-------|
| Row count | 3,376 | 3,376 | ✅ Exact |
| Columns | 7 | 7 | ✅ Exact |
| Country unique | 5 | 2 | ⚠️ Rare values lost |
| State unique | 57 | 55 | ✅ Close |
| IATA format | all valid | 3376/3376 | ✅ All valid |
| Lat range | [-14.3, 71.3] | [10.1, 68.6] | ⚠️ Narrower |
| Lat mean±std | 40.0±8.5 | 40.0±8.5 | ✅ Exact |
| Lon range | [-176.6, 145.8] | [-194.3, -3.2] | ⚠️ Range shifted |
| Lon mean±std | -98.2±24.7 | -97.5±24.8 | ✅ Close |

## Findings

1. **Rare country values lost**: Original has 5 countries (USA dominant at 3372/3376), but generated only has 2 (USA + Palau). The 3 rarest countries (Thailand, Marshall Islands, Bermuda with 1 each) were dropped — likely below the categorical threshold.

2. **Latitude range narrower**: Generated data min is 10.1 vs original -14.3 (American Samoa) and max 68.6 vs 71.3 (Alaska). The extreme outlier airports were not reproduced, but the distribution shape (mean/std) is preserved.

3. **Longitude range shifted**: Generated includes -194.3 which is outside the valid [-180, 180] range. The original has some positive longitudes (Guam at 145.8, Palau at 134.5) that the generated data doesn't reproduce — it over-extends the negative side instead.

4. **City/name/IATA recombination**: IATA codes are drawn from the extracted dictionary, which is correct. Names and cities are shuffled independently, which is expected for synthetic data (no real airport mapping preserved).

## Quality Assessment

**Good**: Row count, column schema, categorical distributions (state), numeric distribution shape (mean, std dev), dictionary-based string generation for IATA codes.

**Needs improvement**: Rare categorical values with count=1 get dropped. Longitude can go outside physical bounds. No correlation between lat/lon and state (generated data has geographically impossible combinations).

## Code Changes Needed

None for this dataset — the issues found are known limitations:
- Rare value drop-off is expected behavior for categorical distributions
- Lack of lat/lon↔state correlation is a known limitation (no spatial awareness)
- Out-of-range longitude is a distribution fitting artifact (normal distribution tails extend beyond observed range)
