<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <title>Pliego Laravel synthetic invoice</title>
    @if (($rehearsalMode ?? null) === 'live' || ($rehearsalMode ?? null) === 'denial')
      <link rel="stylesheet" href="{{ $rehearsalCssUrl }}">
    @endif
    <style>
      @if (($rehearsalMode ?? null) !== 'live')
        @font-face {
          font-family: Ahem;
          src: url("assets/{{ $rehearsalFontFile ?? 'Ahem.ttf' }}")@isset($rehearsalFontFormat) format("{{ $rehearsalFontFormat }}")@endisset;
        }
      @endif
      html, body { margin: 0; }
      body { font: 12px/16px Ahem; }
      @if (($rehearsalMode ?? null) === 'live')
        body.sample { font-size: 12px; line-height: 16px; }
      @endif
      table {
        border-collapse: collapse;
        table-layout: fixed;
        width: 540px;
      }
      caption { height: 32px; text-align: left; }
      th, td {
        border: 1px solid #243447;
        box-sizing: border-box;
        padding: 4px;
        text-align: left;
      }
      thead tr { height: 32px; }
      tbody tr { height: 36px; }
      tfoot tr { height: 40px; }
      th:nth-child(1), td:nth-child(1) { width: 96px; }
      th:nth-child(2), td:nth-child(2) { width: 72px; }
      th:nth-child(3), td:nth-child(3) { width: 276px; }
      th:nth-child(4), td:nth-child(4) { width: 96px; }
      .page-end { break-after: page; }
    </style>
  </head>
  <body{!! (($rehearsalMode ?? null) === 'live') ? ' class="sample"' : '' !!}>
    <table id="invoice">
      <caption>INVOICE PLG-2026-001</caption>
      <thead>
        <tr><th>ITEM</th><th>QTY</th><th>DESCRIPTION</th><th>AMOUNT</th></tr>
      </thead>
      <tbody>
        @foreach ($rows as $row)
          <tr @class(['page-end' => $row === 16])>
            <td>INV-{{ str_pad((string) $row, 3, '0', STR_PAD_LEFT) }}</td>
            <td>1</td>
            <td>SERVICE-{{ str_pad((string) $row, 3, '0', STR_PAD_LEFT) }}</td>
            <td>{{ $row * 10 }}.00</td>
          </tr>
        @endforeach
      </tbody>
      <tfoot>
        <tr><td>TOTAL</td><td>32</td><td>MXN</td><td>5280.00</td></tr>
      </tfoot>
    </table>
    @if (($rehearsalMode ?? null) === 'timeout')
      <script>window.pliego?.defer();</script>
    @endif
  </body>
</html>
