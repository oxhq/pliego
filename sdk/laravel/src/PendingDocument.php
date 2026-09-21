<?php

declare(strict_types=1);

namespace Pliego\Laravel;

use BadMethodCallException;
use Illuminate\Contracts\Filesystem\Factory as FilesystemFactory;
use Illuminate\Contracts\View\Factory as ViewFactory;
use InvalidArgumentException;
use Pliego\Laravel\Exception\DocumentStorageException;
use Pliego\Php\DocumentEngine;
use Pliego\Php\RenderOptions;
use Pliego\Php\RenderResult;
use RuntimeException;
use Symfony\Component\HttpFoundation\BinaryFileResponse;
use Throwable;

/**
 * Blade-to-one-shot API 2 render request.
 */
final class PendingDocument
{
    /** @var array<string, string> */
    private array $assets = [];

    private string $locale;

    private string $timezone;

    private string $pageSize;

    private string $pageMargins;

    /**
     * @param  array<string, mixed>  $data
     */
    public function __construct(
        private readonly ViewFactory $views,
        private readonly DocumentEngine $engine,
        RenderOptions $defaults,
        private readonly string $view,
        private readonly array $data,
        private readonly ?FilesystemFactory $filesystems = null,
        private readonly ?string $defaultStorageDisk = null,
    ) {
        $this->locale = $defaults->locale;
        $this->timezone = $defaults->timezone;
        $this->pageSize = $defaults->pageSize;
        $this->pageMargins = $defaults->pageMargins;
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
        return $this;
    }

    /**
     * @deprecated API 2 profile-null denies live network access. Prefetch and pass the resource to asset().
     */
    public function allowHttpRoot(string $url): self
    {
        throw new BadMethodCallException(
            'Pliego API 2 denies live network access; prefetch the resource and pass it to asset()',
        );
    }

    public function asset(string $bundlePath, string $source): self
    {
        $this->assets[$bundlePath] = $source;

        return $this;
    }

    public function render(): RenderResult
    {
        $totalStartedAt = hrtime(true);
        $viewStartedAt = hrtime(true);
        $html = $this->views->make($this->view, $this->data)->render();
        $viewFinishedAt = hrtime(true);

        return $this->engine->render(
            $html,
            new RenderOptions(
                locale: $this->locale,
                timezone: $this->timezone,
                pageSize: $this->pageSize,
                pageMargins: $this->pageMargins,
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
        if ($filename === '' || basename($filename) !== $filename || str_contains($filename, "\0")) {
            throw new InvalidArgumentException('PDF filename must be a plain file name');
        }
        $result = $this->render();

        return response()->download(
            $result->pdfPath,
            $filename,
            ['Content-Type' => 'application/pdf'],
        );
    }

    /**
     * @param  array<string, mixed>  $options
     */
    public function store(
        string $path,
        ?string $disk = null,
        array $options = [],
    ): StoredDocument {
        if ($path === '' || str_contains($path, "\0")) {
            throw new InvalidArgumentException('PDF storage path is invalid');
        }
        $disk ??= $this->defaultStorageDisk;
        if ($disk === null) {
            throw new InvalidArgumentException('PDF storage disk is not configured');
        }
        if ($disk === '' || str_contains($disk, "\0")) {
            throw new InvalidArgumentException('PDF storage disk is invalid');
        }

        $result = $this->render();

        try {
            if ($this->filesystems === null) {
                throw new RuntimeException('Laravel filesystem storage is not available');
            }

            $stream = @fopen($result->pdfPath, 'rb');
            if (! is_resource($stream)) {
                throw new RuntimeException("cannot open rendered PDF {$result->pdfPath}");
            }

            try {
                if (! $this->filesystems->disk($disk)->writeStream($path, $stream, $options)) {
                    throw new RuntimeException('filesystem write returned false');
                }
            } finally {
                if (is_resource($stream)) {
                    fclose($stream);
                }
            }
        } catch (Throwable $error) {
            throw new DocumentStorageException($disk, $path, $result, $error);
        }

        return new StoredDocument($disk, $path, $result);
    }
}
