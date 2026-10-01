param(
    [string]$ProjectRoot = (Split-Path -Parent $PSScriptRoot)
)

$ErrorActionPreference = 'Stop'
$cli = Join-Path $ProjectRoot 'target\debug\docxtpl.exe'
if (-not (Test-Path -LiteralPath $cli -PathType Leaf)) {
    throw "Build docxtpl-cli before running the Word COM check"
}

$cases = @(
    @{ Name = 'r2_var_basic'; Tables = 0; Rows = 0; Cells = 0 },
    @{ Name = 'r3_tr_for'; Tables = 1; Rows = 3; Cells = 6 },
    @{ Name = 'r3_tr_for_empty'; Tables = 1; Rows = 1; Cells = 2 },
    @{ Name = 'r3_tc_for'; Tables = 1; Rows = 1; Cells = 3 },
    @{ Name = 'r3_vm'; Tables = 1; Rows = 4; Cells = 8 },
    @{ Name = 'r3_hm'; Tables = 1; Rows = 3; Cells = 8 },
    @{ Name = 'r3_nested_tables'; Tables = 3; Rows = 6; Cells = 6 },
    @{ Name = 'r3_combo_invoice'; Tables = 1; Rows = 3; Cells = 9 }
)

$runRoot = Join-Path $ProjectRoot ("target\word-com-check\" + [guid]::NewGuid().ToString('N'))
$renderedRoot = Join-Path $runRoot 'rendered'
$resavedRoot = Join-Path $runRoot 'resaved'
[IO.Directory]::CreateDirectory($renderedRoot) | Out-Null
[IO.Directory]::CreateDirectory($resavedRoot) | Out-Null

foreach ($case in $cases) {
    $template = Join-Path $ProjectRoot ("tests\fixtures\templates\" + $case.Name + '.docx')
    $context = Join-Path $ProjectRoot ("tests\fixtures\contexts\" + $case.Name + '.json')
    $output = Join-Path $renderedRoot ($case.Name + '.docx')
    & $cli render $template $context $output
    if ($LASTEXITCODE -ne 0) {
        throw "Rendering failed for $($case.Name)"
    }
}

$word = $null
try {
    $word = New-Object -ComObject Word.Application
    $word.Visible = $false
    $word.DisplayAlerts = 0
    # msoAutomationSecurityForceDisable: candidate validation must never run
    # document macros while Word opens unattended fixtures.
    $word.AutomationSecurity = 3
    foreach ($case in $cases) {
        $input = Join-Path $renderedRoot ($case.Name + '.docx')
        $output = Join-Path $resavedRoot ($case.Name + '.docx')
        $document = $word.Documents.Open($input, $false, $true, $false)
        try {
            $document.SaveAs2($output, 16)
        }
        finally {
            $document.Close($false)
            [Runtime.InteropServices.Marshal]::FinalReleaseComObject($document) | Out-Null
        }
    }
}
finally {
    if ($null -ne $word) {
        $word.Quit()
        [Runtime.InteropServices.Marshal]::FinalReleaseComObject($word) | Out-Null
    }
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
foreach ($case in $cases) {
    $path = Join-Path $resavedRoot ($case.Name + '.docx')
    $archive = [IO.Compression.ZipFile]::OpenRead($path)
    try {
        $entry = $archive.GetEntry('word/document.xml')
        if ($null -eq $entry) {
            throw "$($case.Name) is missing word/document.xml"
        }
        $reader = [IO.StreamReader]::new($entry.Open())
        try {
            [xml]$xml = $reader.ReadToEnd()
        }
        finally {
            $reader.Dispose()
        }
    }
    finally {
        $archive.Dispose()
    }

    $namespace = [Xml.XmlNamespaceManager]::new($xml.NameTable)
    $namespace.AddNamespace('w', 'http://schemas.openxmlformats.org/wordprocessingml/2006/main')
    $actual = @(
        $xml.SelectNodes('//w:tbl', $namespace).Count,
        $xml.SelectNodes('//w:tr', $namespace).Count,
        $xml.SelectNodes('//w:tc', $namespace).Count
    )
    $expected = @($case.Tables, $case.Rows, $case.Cells)
    if (($actual -join ',') -ne ($expected -join ',')) {
        throw "$($case.Name): expected $($expected -join ','), got $($actual -join ',')"
    }
    Write-Output "$($case.Name): Word opened and resaved; tables/rows/cells = $($actual -join '/')"
}

Write-Output "Word COM check passed; outputs: $resavedRoot"
