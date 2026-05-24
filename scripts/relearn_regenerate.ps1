#!/usr/bin/env pwsh
# Re-learn and re-generate all datasets with the latest knit binary
param(
    [string]$KnitBin = ".\target\release\knit.exe",
    [string]$DatasetsDir = ".\datasets"
)

$ErrorActionPreference = "Continue"
$datasets = Get-ChildItem -Path $DatasetsDir -Directory | Sort-Object Name

$results = @()
$failed = @()
$skipped = @()

foreach ($ds in $datasets) {
    $name = $ds.Name
    $dir = $ds.FullName
    
    # Find original file
    $origCsv = Join-Path $dir "original.csv"
    $origJson = Join-Path $dir "original.json"
    $origJsonl = Join-Path $dir "original.jsonl"
    
    $origFile = $null
    if (Test-Path $origCsv) { $origFile = $origCsv }
    elseif (Test-Path $origJson) { $origFile = $origJson }
    elseif (Test-Path $origJsonl) { $origFile = $origJsonl }
    
    if (-not $origFile) {
        Write-Host "SKIP $name (no original file)" -ForegroundColor Yellow
        $skipped += $name
        continue
    }
    
    $blueprint = Join-Path $dir "blueprint.knit.json"
    $generated = Join-Path $dir "generated.csv"
    
    # Step 1: Learn
    Write-Host "LEARN $name..." -NoNewline
    $learnOut = & $KnitBin learn $origFile -o $blueprint --format csv 2>&1
    if ($LASTEXITCODE -eq 0) {
        Write-Host " OK" -ForegroundColor Green
    } else {
        Write-Host " FAIL" -ForegroundColor Red
        $failed += @{ Name=$name; Phase="learn"; Error=($learnOut -join "`n") }
        continue
    }
    
    # Step 2: Generate
    Write-Host "GEN  $name..." -NoNewline
    $genOut = & $KnitBin generate $blueprint --format csv -o $generated --seed 42 2>&1
    if ($LASTEXITCODE -eq 0) {
        Write-Host " OK" -ForegroundColor Green
        $results += @{ Name=$name; Status="OK" }
    } else {
        Write-Host " FAIL" -ForegroundColor Red
        $failed += @{ Name=$name; Phase="generate"; Error=($genOut -join "`n") }
    }
}

Write-Host "`n=== Summary ==="
Write-Host "Total: $($datasets.Count)"
Write-Host "Succeeded: $($results.Count)" -ForegroundColor Green
Write-Host "Failed: $($failed.Count)" -ForegroundColor Red
Write-Host "Skipped: $($skipped.Count)" -ForegroundColor Yellow

if ($failed.Count -gt 0) {
    Write-Host "`nFailed datasets:"
    foreach ($f in $failed) {
        $errLines = ($f.Error -split "`n") | Select-Object -First 3
        Write-Host "  $($f.Name) ($($f.Phase)):" -ForegroundColor Red
        foreach ($l in $errLines) { Write-Host "    $l" }
    }
}
