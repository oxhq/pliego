<?php

declare(strict_types=1);

namespace Pliego\Laravel\Experimental;

use Illuminate\Contracts\View\Factory as ViewFactory;
use Illuminate\Support\ServiceProvider;
use Pliego\Php\Experimental\CliRenderer;
use Pliego\Php\Experimental\RenderOptions;

final class PliegoServiceProvider extends ServiceProvider
{
    public function register(): void
    {
        $this->mergeConfigFrom(dirname(__DIR__).'/config/pliego.php', 'pliego');

        $this->app->singleton(CliRenderer::class, function ($app): CliRenderer {
            return new CliRenderer(
                [(string) $app['config']->get('pliego.binary')],
                (int) $app['config']->get('pliego.timeout_seconds'),
            );
        });
        $this->app->singleton(DocumentFactory::class, function ($app): DocumentFactory {
            return new DocumentFactory(
                $app->make(ViewFactory::class),
                $app->make(CliRenderer::class),
                (string) $app['config']->get('pliego.work_dir'),
                new RenderOptions(
                    locale: (string) $app['config']->get('pliego.locale'),
                    timezone: (string) $app['config']->get('pliego.timezone'),
                    pageSize: (string) $app['config']->get('pliego.page_size'),
                    pageMargins: (string) $app['config']->get('pliego.page_margins'),
                ),
            );
        });
    }
}
