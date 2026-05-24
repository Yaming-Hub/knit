# Test the column index mapping formula
function Test-ColumnMapping {
    param($primary_idx, $num_fields)
    
    Write-Host ""
    Write-Host "=== Test: $num_fields fields, primary_idx=$primary_idx ==="
    
    # Show TSV column order
    $tsv_order = @($primary_idx)
    for ($i = 0; $i -lt $num_fields; $i++) {
        if ($i -ne $primary_idx) {
            $tsv_order += $i
        }
    }
    Write-Host "TSV column order (original indices): [$($tsv_order -join ', ')]"
    
    # Test each non-primary column's mapping
    $all_pass = $true
    for ($col_idx = 0; $col_idx -lt $num_fields; $col_idx++) {
        if ($col_idx -eq $primary_idx) { 
            Write-Host "col_idx=$col_idx : SKIPPED (primary)"
            continue 
        }
        
        $tsv_col = if ($col_idx -lt $primary_idx) { $col_idx + 1 } else { $col_idx }
        
        # Find actual position in TSV
        $actual_pos = [array]::IndexOf($tsv_order, $col_idx)
        
        if ($tsv_col -eq $actual_pos) {
            Write-Host "col_idx=$col_idx : tsv_col=$tsv_col, actual TSV position=$actual_pos [OK]"
        } else {
            Write-Host "col_idx=$col_idx : tsv_col=$tsv_col, actual TSV position=$actual_pos [WRONG!]"
            $all_pass = $false
        }
    }
    
    if ($all_pass) {
        Write-Host "Result: PASS"
    } else {
        Write-Host "Result: FAIL"
    }
}

# Test the example case
Test-ColumnMapping -primary_idx 2 -num_fields 5

# Edge case: primary at start
Test-ColumnMapping -primary_idx 0 -num_fields 5

# Edge case: primary at end
Test-ColumnMapping -primary_idx 4 -num_fields 5

# Edge case: only 2 fields
Test-ColumnMapping -primary_idx 0 -num_fields 2
Test-ColumnMapping -primary_idx 1 -num_fields 2

# Edge case: 3 fields with primary in middle
Test-ColumnMapping -primary_idx 1 -num_fields 3
