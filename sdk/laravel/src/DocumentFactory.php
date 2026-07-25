<?php

declare(strict_types=1);

namespace Pliego\Laravel\Experimental;

use Illuminate\Contracts\View\Factory as ViewFactory;
use Pliego\Php\Experimental\CliRenderer;
use Pliego\Php\Experimental\RenderOptions;

final readonly class DocumentFactory
{
    public function __construct(
        private ViewFactory $views,
        private CliRenderer $renderer,
        private string $workDirectory,
        private RenderOptions $defaults,
    ) {}

    /**
     * @param array<string, mixed> $data
     */
    public function view(string $name, array $data = []): PendingDocument
    {
        return new PendingDocument(
            $this->views,
            $this->renderer,
            $this->workDirectory,
            $this->defaults,
            $name,
            $data,
        );
    }
}
