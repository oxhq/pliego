<?php

declare(strict_types=1);

namespace Pliego\Laravel;

use Illuminate\Contracts\View\Factory as ViewFactory;
use InvalidArgumentException;
use Pliego\Php\CliRenderer;
use Pliego\Php\RenderOptions;
use Pliego\Php\RenderResult;
use RuntimeException;
use Symfony\Component\HttpFoundation\BinaryFileResponse;

/**
 * Blade-to-one-shot-CLI render request.
 */
final class PendingDocument
{
    /** @var array<string, string> */
    private array $assets = [];

    /** @var list<string> */
    private array $allowedHttpRoots;

    private string $locale;
    private string $timezone;
    private string $pageSize;
    private string $pageMargins;

    /**
     * @param array<string, mixed> $data
     */
    public function __construct(
        private readonly ViewFactory $views,
        private readonly CliRenderer $renderer,
        private readonly string $workDirectory,
        RenderOptions $defaults,
        private readonly string $view,
        private readonly array $data,
    ) {
        $this->locale = $defaults->locale;
        $this->timezone = $defaults->timezone;
        $this->pageSize = $defaults->pageSize;
        $this->pageMargins = $defaults->pageMargins;
        $this->allowedHttpRoots = $defaults->allowedHttpRoots;
    }

    public function pageSize(string $value): self
    {
        $this->pageSize = $value;

        return $this;
    }

    public function margins(string $value): self
    {
        $this->pageMargins = $value;

        return $this;
    }

    public function locale(string $value): self
    {
        $this->locale = $value;

        return $this;
    }

    public function timezone(string $value): self
    {
        $this->timezone = $value;

        return $this;
    }

    public function denyNetwork(): self
    {
        $this->allowedHttpRoots = [];

        return $this;
    }

    public function allowHttpRoot(string $url): self
    {
        $this->allowedHttpRoots[] = $url;

        return $this;
    }

    public function asset(string $bundlePath, string $source): self
    {
        $this->assets[$bundlePath] = $source;

        return $this;
    }

    public function render(string $filename = 'document.pdf'): RenderResult
    {
        $totalStartedAt = hrtime(true);
        if ($filename === '' || basename($filename) !== $filename || str_contains($filename, "\0")) {
            throw new InvalidArgumentException('PDF filename must be a plain file name');
        }
        if (!is_dir($this->workDirectory) && !@mkdir($this->workDirectory, 0700, true)) {
            throw new RuntimeException("cannot create Pliego work directory {$this->workDirectory}");
        }

        $job = rtrim($this->workDirectory, '/\\').DIRECTORY_SEPARATOR.bin2hex(random_bytes(16));
        if (!@mkdir($job, 0700)) {
            throw new RuntimeException("cannot create Pliego job directory {$job}");
        }
        $viewStartedAt = hrtime(true);
        $html = $this->views->make($this->view, $this->data)->render();
        $viewFinishedAt = hrtime(true);

        return $this->renderer->render(
            $html,
            "{$job}/input",
            "{$job}/{$filename}",
            "{$job}/artifacts",
            new RenderOptions(
                locale: $this->locale,
                timezone: $this->timezone,
                pageSize: $this->pageSize,
                pageMargins: $this->pageMargins,
                allowedHttpRoots: array_values(array_unique($this->allowedHttpRoots)),
            ),
            $this->assets,
            bridgeContext: [
                'total_started_ns' => $totalStartedAt,
                'laravel_setup_ns' => $viewStartedAt - $totalStartedAt,
                'view_render_ns' => $viewFinishedAt - $viewStartedAt,
            ],
        );
    }

    public function download(string $filename = 'document.pdf'): BinaryFileResponse
    {
        $result = $this->render($filename);

        return response()->download(
            $result->pdfPath,
            $filename,
            ['Content-Type' => 'application/pdf'],
        );
    }
}
