<?php

declare(strict_types=1);

namespace Pliego\Laravel;

use Illuminate\Contracts\Filesystem\Factory as FilesystemFactory;
use Illuminate\Contracts\View\Factory as ViewFactory;
use Pliego\Php\DocumentEngine;
use Pliego\Php\RenderOptions;

final readonly class DocumentFactory
{
    public function __construct(
        private ViewFactory $views,
        private DocumentEngine $engine,
        private RenderOptions $defaults,
        private ?FilesystemFactory $filesystems = null,
        private ?string $defaultStorageDisk = null,
    ) {}

    /**
     * @param  array<string, mixed>  $data
     */
    public function view(string $name, array $data = []): PendingDocument
    {
        return new PendingDocument(
            $this->views,
            $this->engine,
            $this->defaults,
            $name,
            $data,
            $this->filesystems,
            $this->defaultStorageDisk,
        );
    }
}
