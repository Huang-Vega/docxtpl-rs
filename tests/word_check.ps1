[CmdletBinding()]
param(
    [Parameter(ValueFromPipeline = $true, ValueFromPipelineByPropertyName = $true)]
    [string[]] $Document,

    [string] $OutputDirectory = (Join-Path (Split-Path $PSScriptRoot -Parent) 'target\word-check'),

    [switch] $ExportPdf,

    [switch] $Visible
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$projectRoot = Split-Path $PSScriptRoot -Parent
if (-not $Document -or $Document.Count -eq 0) {
    $Document = @(
        (Join-Path $projectRoot 'tests\oracle\expected\r3_combo_invoice.docx'),
        (Join-Path $projectRoot 'tests\oracle\expected\p4_combo_rich.docx'),
        (Join-Path $projectRoot 'tests\oracle\expected\p5_hf_image.docx'),
        (Join-Path $projectRoot 'tests\oracle\expected\p6_subdoc_image.docx'),
        (Join-Path $projectRoot 'tests\oracle\expected\p7b_word2016.docx')
    )
}

$resolvedOutput = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $resolvedOutput | Out-Null

$word = $null
$wordVersion = $null
$results = [Collections.Generic.List[object]]::new()
$startedAt = [DateTimeOffset]::Now

try {
    $word = New-Object -ComObject Word.Application
    $wordVersion = [string]$word.Version
    $word.Visible = [bool]$Visible
    $word.DisplayAlerts = 0
    $word.Options.SaveNormalPrompt = $false
    $word.Options.ConfirmConversions = $false

    foreach ($item in $Document) {
        $documentPath = [IO.Path]::GetFullPath($item)
        $entryStartedAt = [DateTimeOffset]::Now
        $doc = $null

        try {
            if (-not (Test-Path -LiteralPath $documentPath -PathType Leaf)) {
                throw "Document does not exist: $documentPath"
            }

            # Open read-only, do not add to recent files, and explicitly disable
            # OpenAndRepair. A file that requires repair must fail this gate.
            $doc = $word.Documents.Open(
                $documentPath,
                $false,
                $true,
                $false,
                '',
                '',
                $false,
                '',
                '',
                0,
                0,
                [bool]$Visible,
                $false
            )
            $doc.Repaginate()

            $pdfPath = $null
            if ($ExportPdf) {
                $pdfPath = Join-Path $resolvedOutput (([IO.Path]::GetFileNameWithoutExtension($documentPath)) + '.pdf')
                $doc.ExportAsFixedFormat($pdfPath, 17)
            }

            $results.Add([pscustomobject]@{
                path = $documentPath
                status = 'ok'
                pages = $doc.ComputeStatistics(2)
                words = $doc.ComputeStatistics(0)
                paragraphs = $doc.Paragraphs.Count
                tables = $doc.Tables.Count
                inline_shapes = $doc.InlineShapes.Count
                floating_shapes = $doc.Shapes.Count
                sections = $doc.Sections.Count
                pdf = $pdfPath
                elapsed_ms = [int]([DateTimeOffset]::Now - $entryStartedAt).TotalMilliseconds
                error = $null
            })
        }
        catch {
            $results.Add([pscustomobject]@{
                path = $documentPath
                status = 'error'
                pages = $null
                words = $null
                paragraphs = $null
                tables = $null
                inline_shapes = $null
                floating_shapes = $null
                sections = $null
                pdf = $null
                elapsed_ms = [int]([DateTimeOffset]::Now - $entryStartedAt).TotalMilliseconds
                error = $_.Exception.Message
            })
        }
        finally {
            if ($null -ne $doc) {
                $doc.Close($false)
                [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($doc)
            }
        }
    }
}
catch {
    $results.Add([pscustomobject]@{
        path = $null
        status = 'word_start_error'
        pages = $null
        words = $null
        paragraphs = $null
        tables = $null
        inline_shapes = $null
        floating_shapes = $null
        sections = $null
        pdf = $null
        elapsed_ms = 0
        error = $_.Exception.Message
    })
}
finally {
    if ($null -ne $word) {
        $word.Quit()
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($word)
    }
    [GC]::Collect()
    [GC]::WaitForPendingFinalizers()
}

$report = [ordered]@{
    schema_version = 1
    generated_at = [DateTimeOffset]::Now.ToString('o')
    word_version = $wordVersion
    open_and_repair = $false
    document_count = $Document.Count
    passed = @($results | Where-Object status -eq 'ok').Count
    failed = @($results | Where-Object status -ne 'ok').Count
    elapsed_ms = [int]([DateTimeOffset]::Now - $startedAt).TotalMilliseconds
    documents = $results
}

$reportPath = Join-Path $resolvedOutput 'report.json'
$report | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $reportPath -Encoding utf8
$report | ConvertTo-Json -Depth 5

if ($report.failed -ne 0) {
    exit 1
}
